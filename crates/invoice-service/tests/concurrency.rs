//! Concurrency Test
//! Fire N concurrent POST /pay requests for the same invoice and asserts
//!  that at most one succeeds, no double-charges occur, and the final state
//!  is consistent

mod common;

use common::*;
use sqlx::PgPool;

const CONCURRENT_REQUESTS: usize = 20;

#[sqlx::test]
async fn concurrent_pays_charge_the_card_exactly_once(pool: PgPool) {
    let psp = StubPsp::start(Behaviour::Succeed).await;
    let (base, _state) = spawn_app(pool.clone(), &psp.url).await;
    let invoice_id = seed_invoice(&pool).await;

    // Every request gets a separate idempotency key on purpose. If they
    // shared one, idempotency would mask the race and we'd be testing the
    // wrong mechanism. This leaves only the partial unique index on
    // payment_attempts standing between us and a double charge.
    let mut handles = Vec::new();
    for i in 0..CONCURRENT_REQUESTS {
        let base = base.clone();
        let invoice_id = invoice_id.clone();
        handles.push(tokio::spawn(async move {
            pay(&base, &invoice_id, &format!("race-{i}"), "tok_success")
                .await
                .status()
                .as_u16()
        }));
    }

    let mut statuses = Vec::new();
    for handle in handles {
        statuses.push(handle.await.unwrap());
    }

    let succeeded = statuses.iter().filter(|s| **s == 200).count();
    let conflicted = statuses.iter().filter(|s| **s == 409).count();

    // At most one request may win
    assert_eq!(succeeded, 1, "expected exactly one 200, got {statuses:?}");
    assert_eq!(
        conflicted,
        CONCURRENT_REQUESTS - 1,
        "every loser should get 409, got {statuses:?}"
    );

    assert_eq!(
        psp.charge_calls(),
        1,
        "the processor was called {} times -- that is a double charge",
        psp.charge_calls()
    );

    // Final state is consistent: paid, with exactly one attempt
    assert_eq!(invoice_status(&pool, &invoice_id).await, "paid");
    assert_eq!(attempt_statuses(&pool, &invoice_id).await, vec!["succeeded"]);
}
