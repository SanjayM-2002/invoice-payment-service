//! Emitting and signing webhook events.
//!
//! Here we just writes rows. Actual delivery happens via worker


use chrono::Utc;
use hmac::{Hmac, Mac};
use rand::Rng;
use serde_json::{json, Value};
use sha2::Sha256;
use sqlx::{Postgres, Transaction};

use crate::{error::ApiError, ids::new_id};

/// Build the JSON body that actually gets delivered.
pub fn envelope(id: &str, event_type: &str, created: chrono::DateTime<Utc>, data: &Value) -> Value {
    json!({
        "id": id,
        "type": event_type,
        "created": created,
        "data": data,
    })
}

pub fn sign(secret: &str, timestamp: i64, body: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
        .expect("hmac accepts keys of any length");
    mac.update(format!("{timestamp}.{body}").as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// whsec_ + 32 bytes of CSPRNG
pub fn new_secret() -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut rng = rand::thread_rng();
    let body: String = (0..32)
        .map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char)
        .collect();
    format!("whsec_{body}")
}

/// Write an event and one delivery row per active endpoint.
pub async fn emit_invoice_event(
    tx: &mut Transaction<'_, Postgres>,
    business_id: &str,
    event_type: &str,
    invoice_id: &str,
) -> Result<(), ApiError> {
    // Snapshot the invoice as it is currently
    let invoice = sqlx::query!(
        "SELECT id, customer_id, status, total_cents, currency, due_date, created_at
           FROM invoices WHERE id = $1",
        invoice_id
    )
    .fetch_one(&mut **tx)
    .await?;

    let lines = sqlx::query!(
        "SELECT id, description, quantity, unit_amount_cents
           FROM line_items WHERE invoice_id = $1 ORDER BY sort_order",
        invoice_id
    )
    .fetch_all(&mut **tx)
    .await?;

    let data = json!({
        "invoice": {
            "id": invoice.id,
            "customer_id": invoice.customer_id,
            "status": invoice.status,
            "total_cents": invoice.total_cents,
            "currency": invoice.currency,
            "due_date": invoice.due_date,
            "created_at": invoice.created_at,
            "line_items": lines.iter().map(|l| json!({
                "id": l.id,
                "description": l.description,
                "quantity": l.quantity,
                "unit_amount_cents": l.unit_amount_cents,
                "amount_cents": i64::from(l.quantity) * l.unit_amount_cents,
            })).collect::<Vec<_>>(),
        }
    });

    let event_id = new_id("evt");

    sqlx::query!(
        "INSERT INTO webhook_events (id, business_id, type, payload)
         VALUES ($1, $2, $3, $4)",
        event_id,
        business_id,
        event_type,
        data,
    )
    .execute(&mut **tx)
    .await?;

    let endpoints = sqlx::query!(
        "SELECT id FROM webhook_endpoints WHERE business_id = $1 AND active",
        business_id
    )
    .fetch_all(&mut **tx)
    .await?;

    for endpoint in endpoints {
        sqlx::query!(
            "INSERT INTO webhook_deliveries (id, event_id, endpoint_id, next_attempt_at)
             VALUES ($1, $2, $3, now())",
            new_id("whd"),
            event_id,
            endpoint.id,
        )
        .execute(&mut **tx)
        .await?;
    }

    Ok(())
}
