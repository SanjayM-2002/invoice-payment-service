//! POST /v1/invoices/{id}/pay
//!
//!   1 auth              -> 401
//!   2 claim idempotency -> replay / 422 / 202
//!   3 load invoice      -> 404      (releases the key)
//!   4 state check       -> 409      (releases the key)
//!   5 insert attempt    -> 409      (releases the key)  <- THE LOCK
//!   6 call the PSP      -> 5s timeout, attempt.id as ITS idempotency key
//!   7 resolve           -> one transaction
//!   8 cache response    -> only 200/202
//!
//! Steps 2, 4 and 5 each catch a different kind of duplicate.

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    auth::Business,
    error::ApiError,
    idempotency::{self, Claim},
    ids::new_id,
    psp::ChargeOutcome,
    seed::sha256_hex,
    state::AppState,
    state_machine::InvoiceStatus,
    webhooks,
};

#[derive(Debug, Deserialize)]
pub struct PayRequest {
    pub card_token: String,
}

#[derive(Debug, Serialize)]
pub struct AttemptView {
    pub id: String,
    pub invoice_id: String,
    pub status: String,
    pub amount_cents: i64,
    pub psp_ref: Option<String>,
    pub failure_code: Option<String>,
    pub created_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
}


pub async fn pay(
    State(state): State<AppState>,
    Business(business_id): Business,
    Path(invoice_id): Path<String>,
    headers: HeaderMap,
    body: String,
) -> Result<Response, ApiError> {
    let key = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .ok_or(ApiError::MissingIdempotencyKey)?
        .to_string();


    let request_hash = sha256_hex(&format!("/v1/invoices/{invoice_id}/pay\n{body}"));

    let payload: PayRequest = serde_json::from_str(&body)
        .map_err(|_| ApiError::invalid("card_token is required", Some("card_token")))?;

    // claiming key
    match idempotency::claim(&state.db, &business_id, &key, &request_hash).await? {
        Claim::Replay { status, body } => return Ok((status, Json(body)).into_response()),

        Claim::InProgress { attempt_id } => {
            let attempt = load_attempt(&state, &attempt_id).await?;
            // It may have resolved since; report what's true now.
            let status = if attempt.status == "pending" {
                StatusCode::ACCEPTED
            } else {
                StatusCode::OK
            };
            return Ok((status, Json(json!({ "attempt": attempt }))).into_response());
        }

        Claim::Proceed => {}
    }

    // anything that fails in here must give the key back
    let attempt = match prepare(&state, &business_id, &invoice_id, &key).await {
        Ok(a) => a,
        Err(e) => {
            idempotency::release(&state.db, &business_id, &key).await;
            return Err(e);
        }
    };

    // PSP call
    // attempt.id is the idempotency key, and the only thing we have if the reply never arrives.
    let outcome = state
        .psp
        .charge(&attempt.id, attempt.amount_cents, &payload.card_token)
        .await;

    // resolve
    let (status, attempt) = match outcome {
        ChargeOutcome::Succeeded { psp_ref } => {
            let mut tx = state.db.begin().await?;

            sqlx::query!(
                "UPDATE payment_attempts
                    SET status = 'succeeded', psp_ref = $1, resolved_at = now()
                  WHERE id = $2",
                psp_ref,
                attempt.id
            )
            .execute(&mut *tx)
            .await?;

            sqlx::query!(
                "UPDATE invoices SET status = 'paid', updated_at = now() WHERE id = $1",
                invoice_id
            )
            .execute(&mut *tx)
            .await?;

            // The outbox: same transaction as the state change, so the
            // notification and the payment land together or not at all.
            webhooks::emit_invoice_event(&mut tx, &business_id, "invoice.paid", &invoice_id)
                .await?;

            tx.commit().await?;
            (StatusCode::OK, load_attempt(&state, &attempt.id).await?)
        }

        ChargeOutcome::Failed { code } => {
            let mut tx = state.db.begin().await?;

            sqlx::query!(
                "UPDATE payment_attempts
                    SET status = 'failed', failure_code = $1, resolved_at = now()
                  WHERE id = $2",
                code,
                attempt.id
            )
            .execute(&mut *tx)
            .await?;

            webhooks::emit_invoice_event(
                &mut tx,
                &business_id,
                "invoice.payment_failed",
                &invoice_id,
            )
            .await?;

            tx.commit().await?;

            (StatusCode::OK, load_attempt(&state, &attempt.id).await?)
        }

        // Attempt stays pending, so the unique index still holds and the
        // invoice stays locked - the card may already have been charged.
        ChargeOutcome::Unknown => {
            tracing::warn!(attempt = %attempt.id, "psp outcome unknown, left pending");
            (StatusCode::ACCEPTED, attempt)
        }
    };

    // cache the answer so a retry replays it
    let body = json!({ "attempt": attempt });
    idempotency::complete(&state.db, &business_id, &key, status, &body, &attempt.id).await?;

    Ok((status, Json(body)).into_response())
}

async fn prepare(
    state: &AppState,
    business_id: &str,
    invoice_id: &str,
    key: &str,
) -> Result<AttemptView, ApiError> {
    // load, scoped to the caller
    let invoice = sqlx::query!(
        "SELECT id, status, total_cents FROM invoices
          WHERE id = $1 AND business_id = $2",
        invoice_id,
        business_id
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)?;

    // check if invoice is payable
    let current = InvoiceStatus::from_db(&invoice.status);
    if !current.is_payable() {
        return Err(ApiError::InvoiceNotPayable {
            current_state: current.as_str().to_string(),
        });
    }

    // The Lock: The partial unique index on (invoice_id) WHERE pending lets
    // exactly one concurrent INSERT through
    // Written before the PSP call so a crash mid-charge leaves proof behind
    let attempt_id = new_id("att");

    let inserted = sqlx::query!(
        "INSERT INTO payment_attempts
             (id, invoice_id, amount_cents, next_reconcile_at)
         VALUES ($1, $2, $3, now() + interval '10 seconds')
         RETURNING id, invoice_id, status, amount_cents, psp_ref,
                   failure_code, created_at, resolved_at",
        attempt_id,
        invoice.id,
        invoice.total_cents,
    )
    .fetch_one(&state.db)
    .await;

    let row = match inserted {
        Ok(row) => row,
        Err(sqlx::Error::Database(e))
            if e.constraint() == Some("one_inflight_attempt_per_invoice") =>
        {
            let existing = sqlx::query!(
                "SELECT id FROM payment_attempts
                  WHERE invoice_id = $1 AND status = 'pending'",
                invoice_id
            )
            .fetch_one(&state.db)
            .await?;

            return Err(ApiError::PaymentAlreadyInProgress {
                attempt_id: existing.id,
            });
        }
        Err(e) => return Err(e.into()),
    };

    // A concurrent retry with the same key learns about this attempt.
    idempotency::attach_attempt(&state.db, business_id, key, &row.id).await?;

    Ok(AttemptView {
        id: row.id,
        invoice_id: row.invoice_id,
        status: row.status,
        amount_cents: row.amount_cents,
        psp_ref: row.psp_ref,
        failure_code: row.failure_code,
        created_at: row.created_at,
        resolved_at: row.resolved_at,
    })
}

async fn load_attempt(state: &AppState, attempt_id: &str) -> Result<AttemptView, ApiError> {
    let row = sqlx::query!(
        "SELECT id, invoice_id, status, amount_cents, psp_ref,
                failure_code, created_at, resolved_at
           FROM payment_attempts WHERE id = $1",
        attempt_id
    )
    .fetch_one(&state.db)
    .await?;

    Ok(AttemptView {
        id: row.id,
        invoice_id: row.invoice_id,
        status: row.status,
        amount_cents: row.amount_cents,
        psp_ref: row.psp_ref,
        failure_code: row.failure_code,
        created_at: row.created_at,
        resolved_at: row.resolved_at,
    })
}
