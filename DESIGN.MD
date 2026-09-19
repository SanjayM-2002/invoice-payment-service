# DESIGN.md — Invoice & Payment Service

This is a billing backend that a business runs behind its own product. A business creates customers, issues invoices against them, takes payments through an external payment processor, and receives signed webhooks when an invoice changes.

The API surface covers exactly that and nothing more. Most of the design work sits in the invoice state machine, payment correctness, and handling a payment processor that can be slow, unreachable or silent.

Customers are not authenticated separately. The business owns its customers and calls the API with its own API key, so a payment is always the business's server acting on the customer's behalf. That is also why concurrent `/pay` requests happen at all — a retry or a duplicated job on their side, not two people clicking a button.

---

## 1. Data Model

```text
Business ─┬─< ApiKey
          ├─< Customer ─< Invoice ─┬─< LineItem
          │                        └─< PaymentAttempt
          ├─< WebhookEndpoint
          ├─< WebhookEvent ─< WebhookDelivery
          └─< IdempotencyKey
```

| Table                | Important columns                                                                                  |
| -------------------- | -------------------------------------------------------------------------------------------------- |
| `businesses`         | `id, name`                                                                                         |
| `api_keys`           | `business_id, key_hash, key_last4, revoked_at`                                                     |
| `customers`          | `business_id, name, email`                                                                         |
| `invoices`           | `business_id, customer_id, status, total_cents, currency, due_date`                                |
| `line_items`         | `invoice_id, description, quantity, unit_amount_cents`                                             |
| `payment_attempts`   | `invoice_id, status, amount_cents, psp_ref, reconcile_attempts, next_reconcile_at, needs_review`   |
| `idempotency_keys`   | `(business_id, key)`, `request_hash, status, response_code, response_body, attempt_id, expires_at` |
| `webhook_endpoints`  | `business_id, url, secret, active`                                                                 |
| `webhook_events`     | `business_id, type, payload`                                                                       |
| `webhook_deliveries` | `event_id, endpoint_id, status, attempts, next_attempt_at`                                         |

IDs are prefixed UUIDv7 values. UUIDv7 gives us time-ordered IDs, which are friendlier to B-tree indexes than random UUIDv4 values. The same ID can also be used as a cursor for pagination. I avoided auto-incrementing integers because they are guessable and leak how many records a business has.

Money is stored as `BIGINT` in cents. There is no floating-point money calculation. The API does not accept a client-provided invoice total; the server calculates it from the line items using checked arithmetic. The `currency` column is fixed to USD with a check constraint — an integer amount is meaningless without a currency, since minor units differ between currencies, but supporting more than one is out of scope.

Line items are stored in their own table instead of JSONB so their quantity and amount can also have database-level constraints.

Webhook events and deliveries are separate. An event represents what happened, while a delivery represents an attempt to send that event to a particular endpoint. This means the same event ID stays stable across retries.

Each table has the indexes its queries need: a unique index on `api_keys.key_hash` for authentication, `(business_id, status, created_at)` on invoices for the filtered list endpoint, `(business_id, created_at)` on customers, `invoice_id` on line items and payment attempts, `expires_at` on idempotency keys, and `event_id` on webhook deliveries.

Three indexes are partial, and they carry most of the design:

```sql
CREATE UNIQUE INDEX one_inflight_attempt_per_invoice
  ON payment_attempts (invoice_id)
  WHERE status = 'pending';

CREATE INDEX attempts_due
  ON payment_attempts (next_reconcile_at)
  WHERE status = 'pending' AND needs_review = false;

CREATE INDEX deliveries_due
  ON webhook_deliveries (next_attempt_at)
  WHERE status = 'pending';
```

Most payment attempts and deliveries eventually become terminal, so these indexes mainly contain in-flight work.

At much larger scale, I would partition payment attempts, webhook events and deliveries by time and remove old partitions instead of deleting individual rows. Idempotency keys could also be partitioned by day. Workers could run separately from the API, and read replicas could handle list endpoints.

---

## 2. Invoice State Machine

```text
                       POST /invoices
                             |
                             v
                           OPEN
                             |
        +--------------------+--------------------+
        |                    |                    |
  POST /{id}/void      PSP succeeded     POST /{id}/uncollectible
        |                    |                    |
        v                    v                    v
      VOID                 PAID            UNCOLLECTIBLE
   (terminal)           (terminal)                |
                             ^                    |
                             |   PSP succeeded    |
                             +--------------------+
                              customer can still pay
```

`PAID` and `VOID` are terminal states.

A declined payment does **not** change the invoice state. The invoice remains `OPEN`, while a `invoice.payment_failed` webhook is emitted.

Events:

* `invoice.created` — invoice created
* `invoice.paid` — payment succeeded
* `invoice.payment_failed` — payment was declined

There is no `DRAFT` state because this service has no invoice-edit/finalization workflow, and features such as proration and metered usage are out of scope.

State changes go through one transition function. Invalid transitions are rejected with `409` and an error containing the current state.

`UNCOLLECTIBLE` is reversible. A business can write an invoice off and still allow a later payment.

---

## 3. Payment Correctness & Failure Modes

The payment flow is:

```text
1. Authenticate
2. Check/claim idempotency key
3. Load invoice
4. Check invoice state
5. Create pending payment attempt
6. Call PSP (5s timeout)
7. Resolve the attempt
8. Store and return the response
```

An idempotency key is only consumed if it produced a payment attempt. If the request fails at step 3, 4 or 5, the key is released so the caller can retry with it. Only terminal responses (200 and 202) are stored for replay — a transient `409` is not, because it would otherwise keep being replayed after the condition had cleared.

### (a) Two clients call `/pay` for the same invoice at the same instant

Only one pending payment attempt is allowed for an invoice using the partial unique index above.

Both requests can reach the insert, but PostgreSQL allows only one to succeed. The other receives `409 payment_already_in_progress`.

I chose this instead of holding a `FOR UPDATE` lock while calling the PSP. Holding a database transaction open during an external call could exhaust the connection pool when the PSP is slow. Advisory locks also don't give us a durable record of the in-flight operation. Serializable transactions would add retry complexity for the whole endpoint.

### (b) The PSP times out

The PSP client has a 5-second timeout. If it times out, the API returns `202 Accepted` with the attempt:

```json
{
  "attempt": {
    "id": "att_01a0b619b7aa76039fa459eca17be94c",
    "status": "pending",
    "psp_ref": null
  }
}
```

The invoice remains `OPEN`; the payment attempt is `PENDING`. We do not assume that a timeout means the PSP did not charge the customer.

A reconciler checks the PSP for the result using the payment attempt's PSP idempotency key. It retries with backoff:

```text
10s, 20s, 40s, 80s, 160s, 320s, 600s
```

with jitter and a maximum of 10 attempts.

Network errors follow the same path because we cannot reliably distinguish "request never arrived" from "request arrived but the response was lost." Guessing could result in a double charge.

If reconciliation is exhausted, the attempt remains pending and is marked `needs_review`. We do not automatically release the invoice because the payment outcome is still unknown.

The caller can discover the final result through the webhook or by polling the payment/invoice state.

### (c) The PSP returns success but our service crashes before persisting it

The payment attempt is created before calling the PSP, and its ID is sent to the PSP as its idempotency key.

If the PSP succeeds and our service crashes before saving the result, the durable attempt still exists. On reconciliation, we ask the PSP about that same attempt and can safely complete the payment without creating another charge.

The mock PSP therefore records the idempotency key as soon as it receives the request, including an `in_progress` state. This avoids treating a mid-flight lookup as "payment never happened."

### (d) An idempotency key is reused with a different request body

The idempotency table stores a SHA-256 hash of the HTTP method, path and request body.

Reusing a key with a different request returns `422`. This prevents a caller from accidentally getting the result of an unrelated request.

### (e) An invoice in `PAID` state receives another `/pay`

A new request against a paid invoice returns `409 invoice_not_payable`.

Idempotency is checked before the invoice state. This is important because if the original successful response was lost, retrying the same idempotency key should return the original successful response rather than a new `409`.

---

## 4. Webhook Design

A business registers an endpoint with `POST /v1/webhook_endpoints`, which returns a signing secret once. Endpoints are also seeded so the demo runs without setup.

Webhook payloads are signed using HMAC-SHA256:

```text
"{timestamp}.{raw_body}"
```

The header is:

```text
Dodo-Signature: t=<unix_timestamp>,v1=<hex_signature>
```

The raw request bytes are signed rather than re-serialized JSON, and signatures are compared using a constant-time comparison.

A timestamp tolerance of 5 minutes provides replay protection. Each retry gets a new timestamp and signature.

Retries use:

```text
10s, 1m, 5m, 30m, 2h, 6h, 12h
```

with ±20% jitter. That is 8 attempts in total, over roughly a 21-hour window, so a receiver that is down for a few hours still gets the last couple of tries after it recovers. Any non-2xx response is retried, including 4xx responses, because a rolling deploy genuinely serves 404s for a short time.

After the final attempt, the delivery is marked `exhausted` and retained for audit/debugging.

Webhook delivery is at-least-once and can be unordered. The event ID is the deduplication key.

The webhook event is inserted in the same database transaction as the invoice state change. A separate worker then delivers it using `FOR UPDATE SKIP LOCKED`.

This is an outbox-style approach. It prevents a crash between changing the invoice and creating the webhook job, and a slow webhook receiver never blocks the payment API.

Invoice state remains authoritative, so a business can reconcile missed webhooks by querying the invoice.

---

## 5. API Key Model

Keys are generated using 32 bytes of cryptographically secure randomness and have a recognizable `dodo_sk_` prefix.

Only the SHA-256 hash and last four characters are stored. The plaintext key is returned once.

I use SHA-256 rather than bcrypt because these keys are high-entropy random values rather than human passwords. A slow password hash does not add meaningful protection here and would make lookup more expensive.

Keys are sent using:

```text
Authorization: Bearer <key>
```

and never as query parameters.

Revocation sets `revoked_at` rather than deleting the key. Multiple active keys are supported, so rotation can be done by creating a new key, deploying it, and revoking the old one.

Webhook secrets are different: they need to be available for signing, so they cannot simply be stored as hashes. In production they should be encrypted at rest.

A leaked business API key can access that business's data and mutations, including webhook registration. It cannot access another business because every query is scoped by `business_id`. Unknown resources return `404` rather than exposing whether another tenant's resource exists.

---

## 6. What I Cut and Why

1. **Draft/finalization** — no invoice editing, proration or metered usage is required.
2. **Webhook endpoint management/secret rotation** — registration is enough for this scope; full rotation needs dual-secret handling during cutover.
3. **Event replay endpoint** — invoice polling already provides a way to recover missed state.
4. **`last_used_at` on API keys** — useful operationally, but writing on every request is unnecessary for this version.
5. **Redis/SQS job queue** — PostgreSQL tables with `SKIP LOCKED` are sufficient for the expected scale and keep job creation atomic with the database transaction. At sustained very high throughput, a dedicated queue would make sense.

Refunds, partial payments, credit notes, tax, multi-currency and subscriptions are also outside the scope. If I added refunds later, they would be a separate record referencing the payment attempt rather than a change to the invoice state, since a refunded invoice is still an invoice that was paid. Partial payments would need the invoice to track an amount paid separately from its total, which changes what `PAID` means and would need the state machine revisited.

---

## 7. Production Readiness Gap

Three things I would add before treating this as production-ready:

1. **Better observability** — alerts for old pending payment attempts, `needs_review` attempts and exhausted webhook deliveries. A system that requires manual review also needs a way to alert someone about it.

2. **PSP circuit breaker** — during a prolonged PSP outage, repeatedly waiting 5 seconds and creating pending attempts could lock many invoices. After enough consecutive failures, new payment requests should fail fast until the PSP recovers.

3. **Audit log** — the current model records what happened but not which API key/user performed each mutation or from which IP. This would be important for billing disputes and compromised credentials.

Rate limiting was out of scope, but it is the next thing after those three. Right now nothing stops a single API key from saturating the connection pool, which would affect every other business on the service. I would add a per-key token bucket at the edge, with a separate and smaller budget for `/pay`, so a burst of payment traffic cannot starve ordinary reads.
