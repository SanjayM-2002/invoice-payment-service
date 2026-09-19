//! Reconciler - Resolves payment attempts stuck at pending state.
//!
//! It asks the PSP rather than re-sending the charge

use std::time::Duration;

use rand::Rng;
use sqlx::PgPool;

use crate::{psp::ChargeLookup, state::AppState, webhooks};

/// Exponential retry
const BACKOFF_SECONDS: [i32; 10] = [10, 20, 40, 80, 160, 320, 600, 600, 600, 600];
const MAX_RECONCILE_ATTEMPTS: i32 = 10;

const TICK: Duration = Duration::from_secs(10);
const BATCH: i64 = 50;

const CLAIM_SECONDS: i32 = 60;

struct DueAttempt {
    id: String,
    invoice_id: String,
    business_id: String,
    reconcile_attempts: i32,
}

pub fn spawn(state: AppState) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(TICK);
        loop {
            ticker.tick().await;
            if let Err(e) = tick(&state).await {
                // Log and keep ticking - an unhandled error would kill the
                // loop silently and the reconciler would just stop.
                tracing::error!(error = ?e, "reconciler tick failed");
            }
        }
    });
}

pub async fn tick(state: &AppState) -> anyhow::Result<()> {
    let due = claim_due(&state.db).await?;
    if due.is_empty() {
        return Ok(());
    }

    tracing::info!(count = due.len(), "reconciling pending attempts");

    for attempt in due {
        if let Err(e) = resolve(state, &attempt).await {
            tracing::error!(error = ?e, attempt = %attempt.id, "failed to reconcile attempt");
        }
    }
    Ok(())
}

/// Claims a batch and pushes their next check into the future
async fn claim_due(db: &PgPool) -> anyhow::Result<Vec<DueAttempt>> {
    let rows = sqlx::query!(
        "WITH claimed AS (
             UPDATE payment_attempts
            SET next_reconcile_at = now() + ($1::int * interval '1 second')
          WHERE id IN (
              SELECT id FROM payment_attempts
               WHERE status = 'pending'
                 AND needs_review = false
                 AND next_reconcile_at <= now()
               ORDER BY next_reconcile_at
               LIMIT $2
               FOR UPDATE SKIP LOCKED
          )
        RETURNING id, invoice_id, reconcile_attempts
         )
         SELECT c.id, c.invoice_id, c.reconcile_attempts, i.business_id
           FROM claimed c JOIN invoices i ON i.id = c.invoice_id",
        CLAIM_SECONDS,
        BATCH,
    )
    .fetch_all(db)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| DueAttempt {
            id: r.id,
            invoice_id: r.invoice_id,
            business_id: r.business_id,
            reconcile_attempts: r.reconcile_attempts,
        })
        .collect())
}

async fn resolve(state: &AppState, attempt: &DueAttempt) -> anyhow::Result<()> {
    match state.psp.lookup(&attempt.id).await {
        ChargeLookup::Succeeded { psp_ref } => {
            tracing::info!(attempt = %attempt.id, "reconciled: succeeded");
            mark_paid(&state.db, attempt, &psp_ref).await?;
        }

        ChargeLookup::Failed { code } => {
            tracing::info!(attempt = %attempt.id, code, "reconciled: failed");
            mark_failed(&state.db, attempt, &code).await?;
        }

        ChargeLookup::NotFound => {
            tracing::info!(attempt = %attempt.id, "reconciled: never reached psp");
            mark_failed(&state.db, attempt, "never_reached_psp").await?;
        }

        ChargeLookup::InProgress | ChargeLookup::Unreachable => {
            schedule_retry(&state.db, attempt).await?;
        }
    }
    Ok(())
}

async fn mark_paid(db: &PgPool, attempt: &DueAttempt, psp_ref: &str) -> anyhow::Result<()> {
    let mut tx = db.begin().await?;

    sqlx::query!(
        "UPDATE payment_attempts
            SET status = 'succeeded', psp_ref = $1, resolved_at = now()
          WHERE id = $2 AND status = 'pending'",
        psp_ref,
        attempt.id
    )
    .execute(&mut *tx)
    .await?;

    sqlx::query!(
        "UPDATE invoices SET status = 'paid', updated_at = now()
          WHERE id = $1 AND status IN ('open', 'uncollectible')",
        attempt.invoice_id
    )
    .execute(&mut *tx)
    .await?;

    webhooks::emit_invoice_event(&mut tx, &attempt.business_id, "invoice.paid", &attempt.invoice_id)
        .await?;

    tx.commit().await?;
    Ok(())
}

async fn mark_failed(db: &PgPool, attempt: &DueAttempt, code: &str) -> anyhow::Result<()> {
    let mut tx = db.begin().await?;

    sqlx::query!(
        "UPDATE payment_attempts
            SET status = 'failed', failure_code = $1, resolved_at = now()
          WHERE id = $2 AND status = 'pending'",
        code,
        attempt.id
    )
    .execute(&mut *tx)
    .await?;

    webhooks::emit_invoice_event(
        &mut tx,
        &attempt.business_id,
        "invoice.payment_failed",
        &attempt.invoice_id,
    )
    .await?;

    tx.commit().await?;
    Ok(())
}

async fn schedule_retry(db: &PgPool, attempt: &DueAttempt) -> anyhow::Result<()> {
    let n = attempt.reconcile_attempts + 1;

    if n >= MAX_RECONCILE_ATTEMPTS {
        // Reconcile attempts exhausted, but we do not mark the attempt failed - that
        // would unlock the invoice when the card may already have been
        // charged. Stays locked, flagged for a human.
        tracing::error!(
            attempt = %attempt.id,
            invoice = %attempt.invoice_id,
            "reconciliation budget exhausted -- flagged for manual review, invoice remains locked"
        );

        sqlx::query!(
            "UPDATE payment_attempts
                SET needs_review = true, next_reconcile_at = NULL, reconcile_attempts = $1
              WHERE id = $2",
            n,
            attempt.id
        )
        .execute(db)
        .await?;

        return Ok(());
    }

    let base = BACKOFF_SECONDS[(n - 1) as usize];
    let factor = rand::thread_rng().gen_range(0.8..1.2);
    let delay = ((base as f64) * factor).round() as i32;

    tracing::debug!(attempt = %attempt.id, n, delay, "rescheduling reconciliation");

    sqlx::query!(
        "UPDATE payment_attempts
            SET reconcile_attempts = $1,
                next_reconcile_at = now() + ($2::int * interval '1 second')
          WHERE id = $3",
        n,
        delay,
        attempt.id
    )
    .execute(db)
    .await?;

    Ok(())
}
