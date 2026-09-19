//! Webhook event delivery service
//!
//! Find due rows, claim them, do the work,
//! reschedule with backoff, eventually give up.

use std::time::Duration;

use chrono::Utc;
use rand::Rng;
use sqlx::PgPool;

use crate::{state::AppState, webhooks};

/// 8 attempts, roughly a 21 hour budget.
const BACKOFF_SECONDS: [i32; 8] = [10, 60, 300, 1_800, 7_200, 21_600, 43_200, 43_200];
const MAX_ATTEMPTS: i32 = 8;

const TICK: Duration = Duration::from_secs(5);
const BATCH: i64 = 50;
const CLAIM_SECONDS: i32 = 60;
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);

struct DueDelivery {
    id: String,
    attempts: i32,
    url: String,
    secret: String,
    event_id: String,
    event_type: String,
    payload: serde_json::Value,
    created_at: chrono::DateTime<Utc>,
}

pub fn spawn(state: AppState) {
    tokio::spawn(async move {
        let http = reqwest::Client::builder()
            .timeout(HTTP_TIMEOUT)
            .build()
            .expect("failed to build http client");

        let mut ticker = tokio::time::interval(TICK);
        loop {
            ticker.tick().await;
            if let Err(e) = tick(&state, &http).await {
                tracing::error!(error = ?e, "webhook delivery tick failed");
            }
        }
    });
}

async fn tick(state: &AppState, http: &reqwest::Client) -> anyhow::Result<()> {
    let due = claim_due(&state.db).await?;
    if due.is_empty() {
        return Ok(());
    }

    tracing::info!(count = due.len(), "delivering webhooks");

    for delivery in due {
        if let Err(e) = deliver(&state.db, http, &delivery).await {
            tracing::error!(error = ?e, delivery = %delivery.id, "delivery failed unexpectedly");
        }
    }
    Ok(())
}

async fn claim_due(db: &PgPool) -> anyhow::Result<Vec<DueDelivery>> {
    let rows = sqlx::query!(
        "WITH claimed AS (
             UPDATE webhook_deliveries
                SET next_attempt_at = now() + ($1::int * interval '1 second')
              WHERE id IN (
                  SELECT id FROM webhook_deliveries
                   WHERE status = 'pending' AND next_attempt_at <= now()
                   ORDER BY next_attempt_at
                   LIMIT $2
                   FOR UPDATE SKIP LOCKED
              )
             RETURNING id, event_id, endpoint_id, attempts
         )
         SELECT c.id, c.attempts, c.event_id,
                e.url, e.secret,
                ev.type AS event_type, ev.payload, ev.created_at
           FROM claimed c
           JOIN webhook_endpoints e ON e.id = c.endpoint_id
           JOIN webhook_events    ev ON ev.id = c.event_id",
        CLAIM_SECONDS,
        BATCH,
    )
    .fetch_all(db)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| DueDelivery {
            id: r.id,
            attempts: r.attempts,
            url: r.url,
            secret: r.secret,
            event_id: r.event_id,
            event_type: r.event_type,
            payload: r.payload,
            created_at: r.created_at,
        })
        .collect())
}

async fn deliver(
    db: &PgPool,
    http: &reqwest::Client,
    delivery: &DueDelivery,
) -> anyhow::Result<()> {
    let envelope = webhooks::envelope(
        &delivery.event_id,
        &delivery.event_type,
        delivery.created_at,
        &delivery.payload,
    );
    let body = serde_json::to_string(&envelope)?;

    // Signed with current attempt's time, not the event's.
    let timestamp = Utc::now().timestamp();
    let signature = webhooks::sign(&delivery.secret, timestamp, &body);

    let result = http
        .post(&delivery.url)
        .header("Dodo-Signature", format!("t={timestamp},v1={signature}"))
        .header("Content-Type", "application/json")
        .body(body)
        .send()
        .await;

    match result {
        Ok(response) if response.status().is_success() => {
            tracing::info!(delivery = %delivery.id, event = %delivery.event_id, "delivered");
            sqlx::query!(
                "UPDATE webhook_deliveries
                    SET status = 'delivered', delivered_at = now(),
                        attempts = attempts + 1, last_status_code = $1,
                        next_attempt_at = NULL
                  WHERE id = $2",
                response.status().as_u16() as i32,
                delivery.id
            )
            .execute(db)
            .await?;
        }

        // Non-2xx retries
        Ok(response) => {
            let code = response.status().as_u16() as i32;
            schedule_retry(db, delivery, Some(code), None).await?;
        }

        Err(e) => {
            schedule_retry(db, delivery, None, Some(e.to_string())).await?;
        }
    }
    Ok(())
}

async fn schedule_retry(
    db: &PgPool,
    delivery: &DueDelivery,
    status_code: Option<i32>,
    error: Option<String>,
) -> anyhow::Result<()> {
    let n = delivery.attempts + 1;

    if n >= MAX_ATTEMPTS {
        // Delivery exhausted - Businesses could reconcile by polling GET /v1/invoices, which is authoritative.
        tracing::warn!(
            delivery = %delivery.id,
            event = %delivery.event_id,
            url = %delivery.url,
            "delivery budget exhausted -- dead lettered"
        );

        sqlx::query!(
            "UPDATE webhook_deliveries
                SET status = 'exhausted', attempts = $1,
                    next_attempt_at = NULL, last_status_code = $2, last_error = $3
              WHERE id = $4",
            n,
            status_code,
            error,
            delivery.id
        )
        .execute(db)
        .await?;
        return Ok(());
    }

    let base = BACKOFF_SECONDS[(n - 1) as usize];
    let factor = rand::thread_rng().gen_range(0.8..1.2);
    let delay = ((base as f64) * factor).round() as i32;

    tracing::debug!(delivery = %delivery.id, n, delay, status = ?status_code, "retrying webhook");

    sqlx::query!(
        "UPDATE webhook_deliveries
            SET attempts = $1,
                next_attempt_at = now() + ($2::int * interval '1 second'),
                last_status_code = $3, last_error = $4
          WHERE id = $5",
        n,
        delay,
        status_code,
        error,
        delivery.id
    )
    .execute(db)
    .await?;

    Ok(())
}
