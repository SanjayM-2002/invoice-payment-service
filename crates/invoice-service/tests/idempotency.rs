//! Idempotency Test
//!
//! Retry the same request with the same key and asserts that same response is returned without a second PSP call

mod common;

use common::*;
use sqlx::PgPool;

#[sqlx::test]
async fn same_key_replays_without_charging_again(pool: PgPool) {
    let psp = StubPsp::start(Behaviour::Succeed).await;
    let (base, _state) = spawn_app(pool.clone(), &psp.url).await;
    let invoice_id = seed_invoice(&pool).await;

    let first = pay(&base, &invoice_id, "same-key", "tok_success").await;
    let first_status = first.status();
    let first_body: serde_json::Value = first.json().await.unwrap();

    // Retry - Exact same request, exact same key
    let second = pay(&base, &invoice_id, "same-key", "tok_success").await;
    let second_status = second.status();
    let second_body: serde_json::Value = second.json().await.unwrap();

    assert_eq!(first_status, second_status);

    // Byte-identical, including the original timestamps. A replay, not a
    // re-execution that happened to produce a matching answer.
    assert_eq!(first_body, second_body);

    // The contract: no second charge.
    assert_eq!(
        psp.charge_calls(),
        1,
        "the processor was called {} times for one logical request",
        psp.charge_calls()
    );

    assert_eq!(attempt_statuses(&pool, &invoice_id).await.len(), 1);
}

#[sqlx::test]
async fn same_key_with_a_different_request_is_rejected(pool: PgPool) {
    let psp = StubPsp::start(Behaviour::Succeed).await;
    let (base, _state) = spawn_app(pool.clone(), &psp.url).await;
    let invoice_id = seed_invoice(&pool).await;

    pay(&base, &invoice_id, "reused", "tok_success").await;

    // Same key, different body. Replaying the stored response here would tell
    // the caller that a request we never processed had succeeded; a silent
    // wrong answer nothing downstream could ever detect.
    let response = pay(&base, &invoice_id, "reused", "tok_card_declined").await;

    assert_eq!(response.status(), 422);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "idempotency_key_reuse");
}
