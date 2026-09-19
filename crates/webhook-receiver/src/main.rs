//! Webhook Receiver
//!   POST /hooks              verify the signature, log it, 200
//!   POST /hooks/always-fail  return 500
//!   POST /hooks/slow         sleep 10s, 200

use std::time::Duration;

use axum::{extract::State, http::HeaderMap, routing::post, Router};
use hmac::{Hmac, Mac};
use sha2::Sha256;


const TOLERANCE_SECONDS: i64 = 300;

#[derive(Clone)]
struct Secret(String);

async fn hooks(
    State(Secret(secret)): State<Secret>,
    headers: HeaderMap,
    body: String,
) -> (axum::http::StatusCode, &'static str) {
    let header = headers
        .get("dodo-signature")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let (timestamp, signature) = match parse(header) {
        Some(parts) => parts,
        None => {
            println!("  ✗ malformed or missing Dodo-Signature header");
            return (axum::http::StatusCode::BAD_REQUEST, "bad signature header");
        }
    };

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let age = (now - timestamp).abs();

    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(format!("{timestamp}.{body}").as_bytes());
    let expected = hex::encode(mac.finalize().into_bytes());

    let valid = constant_time_eq(expected.as_bytes(), signature.as_bytes());
    let fresh = age <= TOLERANCE_SECONDS;

    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
    let event_id = parsed["id"].as_str().unwrap_or("?");
    let event_type = parsed["type"].as_str().unwrap_or("?");
    let invoice = &parsed["data"]["invoice"];

    println!("──────────────────────────────────────────────────────────");
    println!("POST /hooks");
    println!("  {event_id}  {event_type}");
    println!(
        "  signature: {}   (t={timestamp}, age {age}s{})",
        if valid { "✓ VALID" } else { "✗ INVALID" },
        if fresh { "" } else { " — EXPIRED" }
    );
    if let Some(id) = invoice["id"].as_str() {
        println!(
            "  invoice {id}  status={}  total_cents={}",
            invoice["status"].as_str().unwrap_or("?"),
            invoice["total_cents"]
        );
    }

    if valid && fresh {
        println!("  → 200");
        (axum::http::StatusCode::OK, "ok")
    } else {
        println!("  → 400 rejected");
        (axum::http::StatusCode::BAD_REQUEST, "invalid signature")
    }
}

/// Always fails, so retries and backoff are visible in webhook_deliveries.
async fn always_fail(body: String) -> (axum::http::StatusCode, &'static str) {
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
    println!(
        "POST /hooks/always-fail   {}  → 500 (deliberate)",
        parsed["id"].as_str().unwrap_or("?")
    );
    (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "nope")
}

/// Slow - Takes 10 seconds.
async fn slow(body: String) -> (axum::http::StatusCode, &'static str) {
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
    println!(
        "POST /hooks/slow         {}  → sleeping 10s...",
        parsed["id"].as_str().unwrap_or("?")
    );
    tokio::time::sleep(Duration::from_secs(10)).await;
    println!("POST /hooks/slow         → 200 (finally)");
    (axum::http::StatusCode::OK, "ok")
}

fn parse(header: &str) -> Option<(i64, String)> {
    let mut timestamp = None;
    let mut signature = None;
    for part in header.split(',') {
        match part.trim().split_once('=') {
            Some(("t", v)) => timestamp = v.parse::<i64>().ok(),
            Some(("v1", v)) => signature = Some(v.to_string()),
            _ => {}
        }
    }
    Some((timestamp?, signature?))
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().with_target(false).init();

    let port: u16 = std::env::var("PORT")
        .unwrap_or_else(|_| "9000".to_string())
        .parse()?;

    // Only /hooks verifies, and only for Atlas.
    let secret = Secret(
        std::env::var("ATLAS_WEBHOOK_SECRET")
            .unwrap_or_else(|_| "whsec_test_atlas_9f3a7b2c1d4e5f60".to_string()),
    );

    let app = Router::new()
        .route("/hooks", post(hooks))
        .route("/hooks/always-fail", post(always_fail))
        .route("/hooks/slow", post(slow))
        .with_state(secret);

    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
    println!("webhook receiver listening on http://0.0.0.0:{port}");
    println!("  /hooks             verifies signatures");
    println!("  /hooks/always-fail always 500s");
    println!("  /hooks/slow        sleeps 10s");

    axum::serve(listener, app).await?;
    Ok(())
}
