//! PSP Failure test
//! "Uses tok_timeout or tok_network_error and asserts the invoice is not stuck
//!  in a bad state."
//! Two cases, because "we don't know" resolves in opposite directions and both
//! have to be right:
//!   the charge DID go through  -> invoice must end up paid
//!   the charge never arrived   -> invoice must become payable again

mod common;

use common::*;
use invoice_service::workers::reconciler;
use sqlx::PgPool;

#[sqlx::test]
async fn unknown_outcome_locks_the_invoice_then_reconciles_to_paid(pool: PgPool) {
    // The dangerous case: our call fails, but the card WAS charged.
    let psp = StubPsp::start(Behaviour::FailThenSucceeded).await;
    let (base, state) = spawn_app(pool.clone(), &psp.url).await;
    let invoice_id = seed_invoice(&pool).await;

    let response = pay(&base, &invoice_id, "unknown-1", "tok_network_error").await;

    // 202, not an error: we accepted the payment, we just can't say how it
    // ended. Treating this as a failure would be a lie.
    assert_eq!(response.status(), 202);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["attempt"]["status"], "pending");

    // The invoice is LOCKED while the outcome is unknown - the card may
    // already have been charged, so a second attempt must be impossible.
    assert_eq!(invoice_status(&pool, &invoice_id).await, "open");
    let blocked = pay(&base, &invoice_id, "unknown-2", "tok_success").await;
    assert_eq!(blocked.status(), 409);
    let blocked_body: serde_json::Value = blocked.json().await.unwrap();
    assert_eq!(blocked_body["error"]["code"], "payment_already_in_progress");

    // Still exactly one charge attempt despite two API calls.
    assert_eq!(psp.charge_calls(), 1);

    // Now let the reconciler do its job: ask the processor what happened.
    sqlx::query("UPDATE payment_attempts SET next_reconcile_at = now() - interval '1 second'")
        .execute(&pool)
        .await
        .unwrap();
    reconciler::tick(&state).await.unwrap();

    // It finds out the charge succeeded. Nothing is stuck.
    assert_eq!(invoice_status(&pool, &invoice_id).await, "paid");
    assert_eq!(attempt_statuses(&pool, &invoice_id).await, vec!["succeeded"]);

    // And it asked rather than re-charging.
    assert_eq!(psp.charge_calls(), 1);
}

#[sqlx::test]
async fn unknown_outcome_that_never_reached_the_psp_unlocks_the_invoice(pool: PgPool) {
    // The other direction: the processor genuinely never saw it.
    let psp = StubPsp::start(Behaviour::FailThenNotFound).await;
    let (base, state) = spawn_app(pool.clone(), &psp.url).await;
    let invoice_id = seed_invoice(&pool).await;

    let response = pay(&base, &invoice_id, "lost-1", "tok_network_error").await;
    assert_eq!(response.status(), 202);

    sqlx::query("UPDATE payment_attempts SET next_reconcile_at = now() - interval '1 second'")
        .execute(&pool)
        .await
        .unwrap();
    reconciler::tick(&state).await.unwrap();

    // A 404 from the processor is only safe to act on because it records
    // charges on receipt - it truly means "never seen this".
    assert_eq!(invoice_status(&pool, &invoice_id).await, "open");
    assert_eq!(attempt_statuses(&pool, &invoice_id).await, vec!["failed"]);

    // Unlocked: the customer can pay for real now.
    let retry = pay(&base, &invoice_id, "lost-2", "tok_success").await;
    assert_eq!(retry.status(), 202); // stub still fails the POST
    assert_eq!(psp.charge_calls(), 2);
}
