#![allow(dead_code)]

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};

use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use sqlx::PgPool;

use invoice_service::{build_app, config::Config, psp::PspClient, state::AppState};

pub const API_KEY: &str = "dodo_sk_test_suite_0000000000000000";

/// What the stub processor should do.
#[derive(Clone, Copy, PartialEq)]
pub enum Behaviour {
    /// Charges succeed immediately.
    Succeed,
    /// POST /charges fails, but the charge DID go through - the dangerous
    /// case. A later lookup reports success.
    FailThenSucceeded,
    /// POST /charges fails and the processor never saw it. A lookup 404s.
    FailThenNotFound,
}

pub struct StubPsp {
    pub url: String,
    /// How many times POST /charges was actually hit. This is the assertion
    /// that matters for the concurrency and idempotency tests - rejecting
    /// requests at our API is only half the story.
    charge_calls: Arc<AtomicUsize>,
}

impl StubPsp {
    pub fn charge_calls(&self) -> usize {
        self.charge_calls.load(Ordering::SeqCst)
    }

    pub async fn start(behaviour: Behaviour) -> Self {
        let calls = Arc::new(AtomicUsize::new(0));
        // Keys we've seen, so repeats replay like the real mock does.
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

        let state = (behaviour, calls.clone(), seen);

        let app = Router::new()
            .route("/charges", post(charge))
            .route("/charges/{key}", get(lookup))
            .with_state(state);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        Self {
            url: format!("http://{addr}"),
            charge_calls: calls,
        }
    }
}

type StubState = (Behaviour, Arc<AtomicUsize>, Arc<Mutex<Vec<String>>>);

async fn charge(
    State((behaviour, calls, seen)): State<StubState>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, StatusCode> {
    calls.fetch_add(1, Ordering::SeqCst);

    let key = body["idempotency_key"].as_str().unwrap_or("").to_string();
    seen.lock().unwrap().push(key);

    match behaviour {
        Behaviour::Succeed => Ok(Json(json!({ "status": "succeeded", "psp_ref": "ch_stub" }))),
        Behaviour::FailThenSucceeded | Behaviour::FailThenNotFound => {
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

async fn lookup(
    State((behaviour, _, seen)): State<StubState>,
    Path(key): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let known = seen.lock().unwrap().contains(&key);
    match behaviour {
        Behaviour::Succeed if known => {
            Ok(Json(json!({ "status": "succeeded", "psp_ref": "ch_stub" })))
        }
        Behaviour::FailThenSucceeded if known => {
            Ok(Json(json!({ "status": "succeeded", "psp_ref": "ch_stub_late" })))
        }
        _ => Err(StatusCode::NOT_FOUND),
    }
}

/// Builds the real app against the given pool and stub, on a random port.
pub async fn spawn_app(pool: PgPool, psp_url: &str) -> (String, AppState) {
    let config = Config {
        database_url: String::new(),
        port: 0,
        psp_base_url: psp_url.to_string(),
        webhook_receiver_base_url: "http://localhost:9000".to_string(),
    };

    let state = AppState {
        db: pool,
        psp: PspClient::new(psp_url.to_string()),
        config: Arc::new(config),
    };

    let app = build_app(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    (format!("http://{addr}"), state)
}

/// A business with an API key, a customer, and one open $56 invoice.
pub async fn seed_invoice(pool: &PgPool) -> String {
    let hash = invoice_service::seed::sha256_hex(API_KEY);

    sqlx::query("INSERT INTO businesses (id, name) VALUES ('biz_test', 'Test Co')")
        .execute(pool)
        .await
        .unwrap();

    sqlx::query(
        "INSERT INTO api_keys (id, business_id, key_hash, key_last4)
         VALUES ('ak_test', 'biz_test', $1, '0000')",
    )
    .bind(&hash)
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO customers (id, business_id, name, email)
         VALUES ('cus_test', 'biz_test', 'Bob', 'bob@example.com')",
    )
    .execute(pool)
    .await
    .unwrap();

    let invoice_id = "inv_test".to_string();
    sqlx::query(
        "INSERT INTO invoices (id, business_id, customer_id, total_cents, due_date)
         VALUES ($1, 'biz_test', 'cus_test', 5600, now() + interval '30 days')",
    )
    .bind(&invoice_id)
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO line_items (id, invoice_id, description, quantity, unit_amount_cents, sort_order)
         VALUES ('li_test', $1, 'Hosting', 1, 5600, 0)",
    )
    .bind(&invoice_id)
    .execute(pool)
    .await
    .unwrap();

    invoice_id
}

pub async fn invoice_status(pool: &PgPool, invoice_id: &str) -> String {
    sqlx::query_scalar::<_, String>("SELECT status FROM invoices WHERE id = $1")
        .bind(invoice_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

pub async fn attempt_statuses(pool: &PgPool, invoice_id: &str) -> Vec<String> {
    sqlx::query_scalar::<_, String>(
        "SELECT status FROM payment_attempts WHERE invoice_id = $1 ORDER BY created_at",
    )
    .bind(invoice_id)
    .fetch_all(pool)
    .await
    .unwrap()
}

pub fn client() -> reqwest::Client {
    reqwest::Client::new()
}

pub async fn pay(
    base: &str,
    invoice_id: &str,
    idempotency_key: &str,
    token: &str,
) -> reqwest::Response {
    client()
        .post(format!("{base}/v1/invoices/{invoice_id}/pay"))
        .header("Authorization", format!("Bearer {API_KEY}"))
        .header("Idempotency-Key", idempotency_key)
        .header("Content-Type", "application/json")
        .body(json!({ "card_token": token }).to_string())
        .send()
        .await
        .unwrap()
}
