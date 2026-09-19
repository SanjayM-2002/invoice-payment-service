//! Mock PSP. 
//! Outcomes are decided by the card token.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum ChargeState {
    InProgress,
    Succeeded { psp_ref: String },
    Failed { code: String },
}

/// In memory hashmap
type Store = Arc<Mutex<HashMap<String, ChargeState>>>;

#[derive(Debug, Deserialize)]
struct ChargeRequest {
    idempotency_key: String,
    #[allow(dead_code)]
    amount_cents: i64,
    card_token: String,
}

async fn charge(
    State(store): State<Store>,
    Json(req): Json<ChargeRequest>,
) -> Result<Json<ChargeState>, StatusCode> {
    // first record the payment
    {
        let mut charges = store.lock().unwrap();

        // Replaying the existing record in case of same idempotency_key instead of charging
        if let Some(existing) = charges.get(&req.idempotency_key) {
            tracing::info!(key = %req.idempotency_key, "replaying existing charge");
            return Ok(Json(existing.clone()));
        }

        charges.insert(req.idempotency_key.clone(), ChargeState::InProgress);
    }

    // tok_network_error case - considering this request never got processed
    if req.card_token == "tok_network_error" {
        store.lock().unwrap().remove(&req.idempotency_key);
        tracing::warn!(key = %req.idempotency_key, "simulating network error");
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }

    let (delay_ms, outcome) = match req.card_token.as_str() {
        "tok_success" => (100, succeeded()),
        "tok_insufficient_funds" => (100, failed("insufficient_funds")),
        "tok_card_declined" => (100, failed("card_declined")),

        // Sleeps 30s and then succeeds
        "tok_timeout" => (30_000, succeeded()),

        _ => (100, failed("invalid_token")),
    };

    let key = req.idempotency_key.clone();
    let token = req.card_token.clone();

    let work = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        store.lock().unwrap().insert(key.clone(), outcome.clone());
        tracing::info!(key = %key, token = %token, "charge resolved");
        outcome
    });

    match work.await {
        Ok(outcome) => Ok(Json(outcome)),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}


/// Reconciler hanler - to know status of a payment later
/// for tok_network_error case, mock psp will return 404
async fn get_charge(
    State(store): State<Store>,
    Path(key): Path<String>,
) -> Result<Json<ChargeState>, StatusCode> {
    store
        .lock()
        .unwrap()
        .get(&key)
        .cloned()
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}

fn succeeded() -> ChargeState {
    ChargeState::Succeeded {
        psp_ref: format!("ch_{}", Uuid::now_v7().simple()),
    }
}

fn failed(code: &str) -> ChargeState {
    ChargeState::Failed {
        code: code.to_string(),
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().with_target(false).init();

    let port: u16 = std::env::var("PORT")
        .unwrap_or_else(|_| "9100".to_string())
        .parse()?;

    let store: Store = Arc::new(Mutex::new(HashMap::new()));

    let app = Router::new()
        .route("/charges", post(charge))
        .route("/charges/{key}", get(get_charge))
        .with_state(store);

    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
    tracing::info!("mock psp listening on http://0.0.0.0:{port}");

    axum::serve(listener, app).await?;
    Ok(())
}
