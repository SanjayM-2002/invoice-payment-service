use axum::http::StatusCode;
use serde_json::Value;
use sqlx::PgPool;

use crate::error::ApiError;

pub enum Claim {
    Proceed,
    Replay { status: StatusCode, body: Value },
    InProgress { attempt_id: String },
}

/// Claimed before any work happens
pub async fn claim(
    db: &PgPool,
    business_id: &str,
    key: &str,
    request_hash: &str,
) -> Result<Claim, ApiError> {
    // Insert if new. If a row exists but has EXPIRED (24h), take it over and
    // reset it; an expired key should behave like a fresh one.
    let claimed = sqlx::query!(
        "INSERT INTO idempotency_keys
             (business_id, key, request_hash, status, expires_at)
         VALUES ($1, $2, $3, 'in_progress', now() + interval '24 hours')
         ON CONFLICT (business_id, key) DO UPDATE
            SET request_hash  = EXCLUDED.request_hash,
                status        = 'in_progress',
                response_code = NULL,
                response_body = NULL,
                attempt_id    = NULL,
                created_at    = now(),
                expires_at    = EXCLUDED.expires_at
          WHERE idempotency_keys.expires_at < now()
         RETURNING key",
        business_id,
        key,
        request_hash,
    )
    .fetch_optional(db)
    .await?;

    if claimed.is_some() {
        return Ok(Claim::Proceed);
    }

    // Somebody already owns a live key with this name
    let existing = sqlx::query!(
        "SELECT request_hash, status, response_code, response_body, attempt_id
         FROM idempotency_keys WHERE business_id = $1 AND key = $2",
        business_id,
        key
    )
    .fetch_one(db)
    .await?;

    // Same key, different request - throw error as it is already used for different request
    if existing.request_hash != request_hash {
        return Err(ApiError::IdempotencyKeyReuse);
    }

    if existing.status == "completed" {
        let status = existing
            .response_code
            .and_then(|c| StatusCode::from_u16(c as u16).ok())
            .unwrap_or(StatusCode::OK);
        return Ok(Claim::Replay {
            status,
            body: existing.response_body.unwrap_or(Value::Null),
        });
    }

    // Still running case
    match existing.attempt_id {
        Some(attempt_id) => Ok(Claim::InProgress { attempt_id }),
        // Very narrow window: claimed, but the attempt row isn't in yet.
        None => Err(ApiError::RequestInProgress),
    }
}

/// A key is only consumed if it produced a payment attempt. Anything that
/// fails earlier, for eg: validation, 404, a state conflict - can be reused
pub async fn release(db: &PgPool, business_id: &str, key: &str) {
    let result = sqlx::query!(
        "DELETE FROM idempotency_keys
         WHERE business_id = $1 AND key = $2 AND status = 'in_progress'",
        business_id,
        key
    )
    .execute(db)
    .await;

    if let Err(e) = result {
        tracing::warn!(error = %e, key, "failed to release idempotency key");
    }
}

/// Record the outcome so a retry replays it. Only for terminal answers (200 and 202)
pub async fn complete(
    db: &PgPool,
    business_id: &str,
    key: &str,
    status: StatusCode,
    body: &Value,
    attempt_id: &str,
) -> Result<(), ApiError> {
    sqlx::query!(
        "UPDATE idempotency_keys
            SET status = 'completed',
                response_code = $1,
                response_body = $2,
                attempt_id = $3
          WHERE business_id = $4 AND key = $5",
        status.as_u16() as i32,
        body,
        attempt_id,
        business_id,
        key,
    )
    .execute(db)
    .await?;
    Ok(())
}

/// Link the key to its attempt as soon as the attempt exists, so a concurrent retry gets InProgress { attempt_id } instead of a bare error.
pub async fn attach_attempt(
    db: &PgPool,
    business_id: &str,
    key: &str,
    attempt_id: &str,
) -> Result<(), ApiError> {
    sqlx::query!(
        "UPDATE idempotency_keys SET attempt_id = $1
          WHERE business_id = $2 AND key = $3",
        attempt_id,
        business_id,
        key
    )
    .execute(db)
    .await?;
    Ok(())
}
