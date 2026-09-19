# API Reference

Base URL: `http://localhost:8080`
All endpoints are prefixed `/v1` except `GET /health`.

---

## Conventions

**Authentication.** Every `/v1` endpoint requires an API key scoped to one business:

```
Authorization: Bearer dodo_sk_test_atlas_a1b2c3d4e5f6g7h8
```

Keys are never accepted as query parameters. Seeded demo keys are in the README.

**Money.** All amounts are **integer USD minor units (cents)**. `1999` means $19.99. No
endpoint accepts or returns a decimal amount. Invoice totals are always computed server-side
from line items — there is no request field in which a client can supply a total.

**Tenancy.** A resource belonging to another business returns `404`, never `403`. A `403`
would confirm the resource exists.

**Timestamps.** RFC 3339 / ISO 8601 UTC strings (`2026-09-18T14:03:22Z`).

**Identifiers.** Prefixed and time-ordered: `cus_`, `inv_`, `li_`, `att_`, `evt_`, `req_`.

**Request ids.** Every response carries `X-Request-Id`. Error bodies repeat it as
`error.request_id`. Quote it in any support conversation.

### Error format

Every error, without exception:

```json
{
  "error": {
    "code": "invoice_not_payable",
    "message": "Invoice inv_01JBQP7K3M9XZ2VNRT8DYF4WCA is already paid.",
    "request_id": "req_01JBQP9M4T8ZJY5LNXR6UA2CWE"
  }
}
```

Some codes add context fields alongside `code`/`message`/`request_id`.

| HTTP | `code` | Meaning | Extra fields |
|---|---|---|---|
| 400 | `invalid_request` | Malformed body or a field that failed validation | `param` |
| 400 | `missing_idempotency_key` | `POST /pay` sent without an `Idempotency-Key` header | |
| 401 | `invalid_api_key` | Key missing, malformed, unknown, or revoked | |
| 404 | `not_found` | No such resource, or it belongs to another business | |
| 409 | `invoice_not_payable` | `POST /pay` on an invoice that is not `open` | `current_state` |
| 409 | `invalid_transition` | `/void` or `/uncollectible` on an invoice that is not `open` | `current_state`, `attempted_state` |
| 409 | `payment_already_in_progress` | A payment attempt for this invoice is already in flight | `attempt_id` |
| 409 | `request_in_progress` | This idempotency key is claimed but has not yet produced an attempt. Retry shortly. | |
| 422 | `idempotency_key_reuse` | This `Idempotency-Key` was already used with a different request | |
| 500 | `internal_error` | Unhandled failure | |

There is deliberately **no `503` for payment-processor problems**. If the processor is slow,
unreachable, or errors, the payment is accepted and resolved asynchronously — see
`POST /invoices/{id}/pay`.

### Pagination

List endpoints accept `limit` (default `25`, max `100`) and `starting_after` (an id from a
previous page) and return:

```json
{ "data": [ ... ], "has_more": true }
```

Ids are time-ordered, so `starting_after` is a true keyset cursor — pagination cost does not
grow with page depth.

---

## Health

### `GET /health`

No authentication. Returns `200 {"status":"ok"}` once the database is reachable. Used by the
Docker Compose healthcheck.

---

## Customers

### `POST /v1/customers`

```json
{ "name": "Bob Smith", "email": "bob@example.com" }
```

`201 Created`

```json
{
  "id": "cus_01JBQP8N2R7YHX3KMWQ5TZ9BVD",
  "name": "Bob Smith",
  "email": "bob@example.com",
  "created_at": "2026-09-18T14:03:22Z"
}
```

| Field | Rules |
|---|---|
| `name` | required, 1–255 chars |
| `email` | required, must contain `@`, max 255 chars |

### `GET /v1/customers/{id}`

`200` with the customer. `404 not_found` if it does not exist or belongs to another business.

### `GET /v1/customers`

Query: `limit`, `starting_after`. Returns the paginated envelope, newest first.

---

## Invoices

### `POST /v1/invoices`

```json
{
  "customer_id": "cus_01JBQP8N2R7YHX3KMWQ5TZ9BVD",
  "due_date": "2026-10-18T00:00:00Z",
  "line_items": [
    { "description": "Server hosting — October", "quantity": 1, "unit_amount_cents": 5000 },
    { "description": "Bandwidth overage",        "quantity": 3, "unit_amount_cents": 200 }
  ]
}
```

`201 Created`

```json
{
  "id": "inv_01JBQP7K3M9XZ2VNRT8DYF4WCA",
  "customer_id": "cus_01JBQP8N2R7YHX3KMWQ5TZ9BVD",
  "status": "open",
  "total_cents": 5600,
  "currency": "USD",
  "due_date": "2026-10-18T00:00:00Z",
  "line_items": [
    { "id": "li_01JBQ…", "description": "Server hosting — October",
      "quantity": 1, "unit_amount_cents": 5000, "amount_cents": 5000 },
    { "id": "li_01JBQ…", "description": "Bandwidth overage",
      "quantity": 3, "unit_amount_cents": 200,  "amount_cents": 600 }
  ],
  "payment_attempts": [],
  "created_at": "2026-09-18T14:03:22Z"
}
```

| Field | Rules |
|---|---|
| `customer_id` | required, must belong to the authenticated business |
| `due_date` | required, RFC 3339 |
| `line_items` | required, 1–100 items |
| `line_items[].description` | required, 1–500 chars |
| `line_items[].quantity` | required integer, 1–10,000 |
| `line_items[].unit_amount_cents` | required integer, 0–100,000,000 |

`total_cents` is the sum of `quantity × unit_amount_cents`, computed with checked arithmetic;
the request has no field for it. The resulting total must be greater than zero.

`amount_cents` on each line item is returned for convenience and is not stored.

### `GET /v1/invoices/{id}`

`200` with the full invoice, including `line_items` and **all** `payment_attempts`. This is how
a caller discovers the eventual result of an asynchronous payment.

### `GET /v1/invoices`

Query: `status` (`open` · `paid` · `void` · `uncollectible`), `limit`, `starting_after`.

Because invoice state is authoritative and webhooks are best-effort, this endpoint is also the
supported way to reconcile missed webhook events.

---

## Payments

### `POST /v1/invoices/{id}/pay`

```
Idempotency-Key: 8f14e45f-ea6a-4c1e-9b2a-6d0c1f7c33aa      (required)
```

```json
{ "card_token": "tok_success" }
```

Tokens recognised by the mock processor: `tok_success`, `tok_insufficient_funds`,
`tok_card_declined`, `tok_timeout`, `tok_network_error`.

**`200 OK` — outcome known**

```json
{
  "attempt": {
    "id": "att_01JBQP9M4T8ZJY5LNXR6UA2CWE",
    "invoice_id": "inv_01JBQP7K3M9XZ2VNRT8DYF4WCA",
    "status": "succeeded",
    "amount_cents": 5600,
    "psp_ref": "ch_7f3a1b2c",
    "failure_code": null,
    "created_at": "2026-09-18T14:05:00Z",
    "resolved_at": "2026-09-18T14:05:00Z"
  },
  "invoice": { "id": "inv_01JBQP7K3M9XZ2VNRT8DYF4WCA", "status": "paid" }
}
```

A declined card also returns `200`, with `attempt.status = "failed"` and a `failure_code` of
`insufficient_funds` or `card_declined`. **A decline is not an HTTP error** — the request
succeeded and produced a definitive answer. The HTTP status describes the API call; the
`status` field describes the money.

**`202 Accepted` — outcome not yet known**

```json
{
  "attempt": {
    "id": "att_01JBQPB6X2N4KDZ8VYW1TR5CHQ",
    "invoice_id": "inv_01JBQP7K3M9XZ2VNRT8DYF4WCA",
    "status": "pending",
    "amount_cents": 5600,
    "psp_ref": null,
    "failure_code": null,
    "created_at": "2026-09-18T14:05:00Z",
    "resolved_at": null
  },
  "invoice": { "id": "inv_01JBQP7K3M9XZ2VNRT8DYF4WCA", "status": "open" }
}
```

Returned when the processor times out or is unreachable. **Do not treat this as a failure, and
do not retry with a new idempotency key.** The card may or may not have been charged. The
invoice cannot be paid again until the attempt resolves; further calls return `409
payment_already_in_progress`.

The attempt is resolved automatically by a background reconciler that queries the processor
with exponential backoff. You will learn the outcome from the `invoice.paid` or
`invoice.payment_failed` webhook, or by polling `GET /v1/invoices/{id}`.

If the outcome remains undeterminable after the retry budget is exhausted, the attempt stays
`pending` and the invoice stays locked pending manual review. It is never resolved by guessing.

**Idempotency.** `Idempotency-Key` is required. Retrying with the same key and the same request
returns the original response, including its status code, without a second charge. Keys expire
after 24 hours and are scoped to the authenticated business. A key is only consumed if it
produced a payment attempt — requests rejected at validation, `404`, or state checks release the
key for reuse. Reusing a key with a *different* request body or path returns `422
idempotency_key_reuse`.

**Errors:** `400 missing_idempotency_key` · `404 not_found` · `409 invoice_not_payable` ·
`409 payment_already_in_progress` · `409 request_in_progress` · `422 idempotency_key_reuse`

---

## State transitions

### `POST /v1/invoices/{id}/void`

Cancels an invoice that should not have been issued. `200` with the updated invoice.
Terminal — a voided invoice can never be paid or altered.

### `POST /v1/invoices/{id}/uncollectible`

Writes the invoice off as bad debt. `200` with the updated invoice. A customer may still pay
afterwards, which moves it to `paid` — the only reversible transition in the system.

Both require the invoice to be `open`; otherwise `409 invalid_transition`:

```json
{
  "error": {
    "code": "invalid_transition",
    "message": "Invoice is paid and cannot be voided.",
    "current_state": "paid",
    "attempted_state": "void",
    "request_id": "req_01JBQ…"
  }
}
```

---

## Webhooks

Businesses receive signed `POST` requests at their registered endpoints.

### `POST /v1/webhook_endpoints`

Registers an endpoint to receive events.

```json
{ "url": "https://acme.dev/hooks/dodo" }
```

`201 Created`

```json
{
  "id": "whe_01JBQPD4K7M2XZ9VNRT5DYF8WC",
  "url": "https://acme.dev/hooks/dodo",
  "secret": "whsec_7Kq2mZvN8xPwR4tYbA3cLgH6"
}
```

| Field | Rules |
|---|---|
| `url` | required, must begin `http://` or `https://` |

The `secret` is returned **exactly once**, at creation — the same show-once
model as an API key. Store it; it cannot be retrieved again. Use it to verify
the `Dodo-Signature` header on every delivery.

Unlike API keys, this secret is stored recoverably rather than hashed, because
producing a signature requires the original bytes.

A demo endpoint is seeded for each business so `docker compose up` is
immediately demonstrable without registering one first.

**Errors:** `400 invalid_request` (bad URL) · `401 invalid_api_key`

### Events

| Type | Fires when |
|---|---|
| `invoice.created` | An invoice is created |
| `invoice.paid` | A payment attempt succeeds (including via reconciliation) |
| `invoice.payment_failed` | A payment attempt is declined |

### Payload

```json
{
  "id": "evt_01JBQPC8M3R5NZX7KVW2QT6DHA",
  "type": "invoice.paid",
  "created": "2026-09-18T14:05:00Z",
  "data": { "invoice": { "id": "inv_…", "status": "paid", "total_cents": 5600, "…": "…" } }
}
```

`data` is a snapshot taken when the event occurred, not when it was delivered.

### Signature

```
Dodo-Signature: t=1758204300,v1=5257a869e7ecebeda32affa62cdca3fa51cad7e77a0e56ff536d0ce8e108d8bd
```

- Algorithm: **HMAC-SHA256** with your endpoint secret
- Signed payload: `"{t}.{raw_request_body}"`
- `t` is the unix time of **this delivery attempt** — retries are signed afresh, so `t` will not
  match the event's `created`
- Reject deliveries where `|now - t| > 300` seconds

```js
const expected = crypto.createHmac('sha256', secret)
                       .update(`${t}.${rawBody}`)
                       .digest('hex')
crypto.timingSafeEqual(Buffer.from(expected), Buffer.from(v1))
```

> ⚠️ Sign the **raw request body bytes**. `JSON.stringify(JSON.parse(body))` is not byte-identical
> to `body` — key order, whitespace and unicode escaping all differ — and verification will fail
> intermittently. Capture the raw body before any JSON middleware parses it.

> ⚠️ Use a constant-time comparison. `===` returns early on the first mismatched character, which
> leaks the signature one byte at a time.

### Delivery guarantees

- **At-least-once.** Deduplicate on the event `id`, which is stable across retries.
- **Unordered.** `invoice.paid` may arrive before `invoice.created`. Treat events as
  notifications and read current state from the API when order matters.
- **Retries:** 8 attempts at 10s, 1m, 5m, 30m, 2h, 6h and 12h with ±20% jitter — a ~21 hour
  budget. Any non-`2xx` response is retried. Respond `2xx` as soon as you have durably recorded
  the event; do your processing afterwards.
- **Exhaustion.** After 8 failures the delivery is abandoned and retained for audit. Reconcile
  by polling `GET /v1/invoices` — invoice state is authoritative and webhooks are not.

---

## Mock Payment Processor

Separate service, not part of the public API. Documented because the invoice service treats it
as a real external dependency.

### `POST /charges`

```json
{ "idempotency_key": "att_01JBQP9M4T8ZJY5LNXR6UA2CWE", "amount_cents": 5600,
  "card_token": "tok_success" }
```

| `card_token` | Behaviour |
|---|---|
| `tok_success` | `200 {"status":"succeeded","psp_ref":"<uuid>"}` after ~100ms |
| `tok_insufficient_funds` | `200 {"status":"failed","code":"insufficient_funds"}` after ~100ms |
| `tok_card_declined` | `200 {"status":"failed","code":"card_declined"}` after ~100ms |
| `tok_timeout` | Sleeps 30s, then succeeds |
| `tok_network_error` | Returns `500` or drops the connection |

The charge record is written **on receipt**, before processing, so it is visible to `GET
/charges/{key}` while still in flight. This is what makes `404` from that endpoint a safe signal
that no charge was ever created.

Repeating a `POST` with the same `idempotency_key` returns the existing record rather than
charging again.

### `GET /charges/{idempotency_key}`

```json
{ "status": "in_progress" }
{ "status": "succeeded", "psp_ref": "ch_7f3a1b2c" }
{ "status": "failed", "code": "card_declined" }
```

`404` if the key has never been seen. Used by the reconciler to resolve `pending` attempts.
