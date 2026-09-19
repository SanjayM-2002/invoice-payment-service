# AI Usage

I used AI tools during this assignment for brainstorming, research, learning Rust conventions, and getting feedback on design decisions.

I was already comfortable with backend development using TypeScript/Node.js, Express/NestJS and PostgreSQL, but I am less experienced with Rust. Because of that, I used AI more heavily for Rust syntax and implementation details than I would normally need to for a Node.js backend.

## Prior Experience

I have previously worked with payment providers and payment APIs professionally. Through that work, I had developed a basic understanding of how payment systems are generally structured — for example, payment attempts, idempotency, webhooks, asynchronous processing, and handling failures from external payment providers.

I had also formed some ideas about how the underlying data models and flows might be structured based on the APIs and documentation I had worked with. These were assumptions/inferences rather than knowledge of the providers' actual internal implementations, so they could of course be incorrect.

That prior experience influenced my initial thinking. I then used AI to challenge those ideas, explore alternative approaches, and work through failure cases rather than treating my assumptions as facts.

## 1. Tools Used, and What For

### Claude

Design and failure-mode work, before any code was written:

* Deciding how reconciliation should resolve an unknown outcome — asking the PSP for the result versus re-sending the charge with the same idempotency key. I chose asking, because a lookup is a read and cannot double-charge, whereas re-sending makes the recovery path capable of causing the exact bug the design exists to prevent.
* Working out that the mock PSP has to record the idempotency key when it *receives* a request rather than when it completes. Otherwise a reconciliation lookup during the in-flight window returns 404, which we would wrongly read as "nothing was charged".
* Reviewing my schema and suggesting refinements — particularly the partial indexes, the CHECK constraints on line item quantity and amount, and the composite primary key on `idempotency_keys` so keys cannot collide across businesses.
* Rust implementation details: Axum routing and extractors, SQLx query macros and migrations, background workers with `tokio::spawn`, and the HTTP client for the PSP.

### ChatGPT

I used ChatGPT earlier on to get comfortable with Rust by mapping it onto things I already know:

* Express/NestJS routes to Axum routes and handlers.
* DTOs to Rust structs.
* Promises and `async/await` to Futures.
* Prisma-style database access to SQLx.
* Middleware and guards to Axum extractors.

This was about getting oriented in the ecosystem before implementing anything.

## 2. Three Decisions I Made Myself

### 1. Not Releasing an Invoice After an Ambiguous PSP Result

For PSP timeouts and network failures, I decided not to automatically mark the payment as failed or allow another payment attempt.

A timeout does not tell us whether the PSP received and processed the payment. Automatically allowing another attempt could therefore result in a double charge.

Instead, the payment attempt remains pending and is reconciled using the same PSP idempotency key. If reconciliation is exhausted, the attempt is marked for manual review.

This was one of the main failure-mode decisions in the design and was based on my understanding of payment-provider behaviour and the requirement's failure scenarios.

### 2. Creating the Payment Attempt Before Calling the PSP

I decided to create the payment attempt before making the external PSP request.

This gives us a durable record of the operation even if the service crashes while the PSP request is in progress. The payment attempt ID is also used as the PSP idempotency key, allowing the outcome to be reconciled later without creating another charge.


### 3. Keeping Uncertainty on the Payment Attempt, Not the Invoice

I decided to keep the invoice as OPEN while a payment is in progress, and let the PENDING payment attempt represent the uncertainty.

An alternative would be a PAYMENT_PROCESSING invoice state. I chose not to introduce it because a crash in that state would require a separate recovery mechanism just to move the invoice forward.

Keeping the invoice state limited to actual outcomes means that "we don't know yet" is a property of the payment attempt, not the invoice.

This also helps with the crash case: the payment attempt is created before the PSP call, so if resolving the payment fails, the attempt remains PENDING and the reconciler can later determine the PSP result.

## 3. What the AI Got Wrong

### 1. The Mock PSP Silently Cancelled Its Own Timeout Charges

The generated mock PSP initially handled `tok_timeout` directly inside the request handler — sleeping for 30 seconds and then recording the payment as successful.

The issue was that when our service timed out after 5 seconds and disconnected, the request handler was dropped. Since the sleep was part of that handler, it was cancelled too, so the payment was never recorded and the PSP remained stuck in `in_progress`.

This mattered because `tok_timeout` exists to simulate a charge that succeeds after the caller has given up waiting. With the sleep being cancelled it was doing the opposite, so the reconciler would never have found a result and the invoice would have ended up locked for manual review.

I noticed this because the expected `charge resolved` log never appeared after the timeout. Nothing crashed or failed to compile; the issue only showed up at runtime.

I fixed it by moving the delayed payment processing into a separate `tokio::spawn` task so it continues even after the request handler is dropped.

### 2. Holding the Invoice Lock During the PSP Call

AI suggested using SELECT ... FOR UPDATE on the invoice and keeping the transaction open while calling the PSP.

I changed this because the PSP is an external dependency and can be slow or unavailable. Holding a database connection and transaction open during the external call could unnecessarily consume the connection pool. I used a pending payment attempt with a database constraint instead.


## Verification

I did not treat generated code as correct. I reviewed it against the requirements and adapted it to the design.

I tested the important behaviours locally rather than assuming them:

* Successful and declined payments, including confirming the mock PSP was called exactly once.
* Repeating a request with the same idempotency key and checking the response was byte-identical, including the original timestamps, with no second PSP call.
* Firing ten concurrent `/pay` requests at one invoice and confirming one `200`, nine `409`, and a single charge in the PSP log.
* `tok_timeout` and `tok_network_error`, following each one through to the reconciler resolving it in opposite directions.
* Invalid state transitions.
* Webhook delivery, signature verification, and retry backoff against a deliberately failing endpoint.

The three required automated tests assert the same things, with the PSP stub counting calls so that "the card was charged once" is an actual assertion rather than an inference from status codes.

The final implementation and design decisions are my responsibility.
