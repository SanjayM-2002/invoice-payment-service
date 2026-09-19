# Invoice & Payment Service

A minimal billing backend: businesses create invoices for their customers,
customers pay them, and businesses are notified by signed webhooks.

Rust · Axum · SQLx · PostgreSQL

## Demo Video

Part 1: https://www.loom.com/share/870838f9cd094d25bb6c5adf53923586

Part 2: https://www.loom.com/share/29e6637022ed4a139d94f0e7438535ed


## Documentation

| Document | What's in it |
|---|---|
| **[DESIGN.md](DESIGN.md)** | The primary deliverable — data model, state machine, payment correctness and failure modes, webhooks, API keys, what was cut, and what's missing for production |
| [API.md](API.md) | Every endpoint, request and response shapes, and the error format |
| [AI_USAGE.md](AI_USAGE.md) | Which AI tools were used and for what, decisions made independently, and what had to be corrected |


---

## Quick start

```bash
docker compose up
```

That's the whole setup. The service applies its own migrations and seeds demo
data on boot — there is no separate migrate step, no seed command.

Four containers come up:

| Service | Port | |
|---|---|---|
| `invoice-service` | 8080 | the API |
| `mock-psp` | 9100 | pretend payment processor |
| `webhook-receiver` | 9000 | **demo only** — logs and verifies incoming webhooks |
| `postgres` | 5433 | |

Check it's alive:

```bash
curl localhost:8080/health
```

### Seeded businesses

Committed on purpose so the demo needs zero setup. Real keys are generated from
a CSPRNG, shown exactly once at creation, and only ever stored as a SHA-256
hash — which is what happens to these too; the database never holds a plaintext
key.

| Business | API key | Its webhook endpoint |
|---|---|---|
| **Atlas** | `dodo_sk_test_atlas_a1b2c3d4e5f6a7b8` | verifies signatures, returns 200 |
| **Nova** | `dodo_sk_test_nova_c1d2e3f4a5b6c7d8` | sleeps 10s (shows delivery is off the response path) |
| **Vertex** | `dodo_sk_test_vertex_e1f2a3b4c5d6e7f8` | always 500s (shows retries and backoff) |

---

## Example usage

```bash
export KEY="dodo_sk_test_atlas_a1b2c3d4e5f6a7b8"
```

### 1. Create a customer

```bash
curl -s -X POST localhost:8080/v1/customers \
  -H "Authorization: Bearer $KEY" \
  -H "Content-Type: application/json" \
  -d '{"name":"Bob Smith","email":"bob@example.com"}'
```

### 2. Create an invoice

The server computes the total. There is no request field for it — a
client-supplied total isn't ignored, it's unrepresentable.

```bash
curl -s -X POST localhost:8080/v1/invoices \
  -H "Authorization: Bearer $KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "customer_id": "cus_REPLACE_ME",
    "due_date": "2026-10-18T00:00:00Z",
    "line_items": [
      { "description": "Server hosting — October", "quantity": 1, "unit_amount_cents": 5000 },
      { "description": "Bandwidth overage",        "quantity": 3, "unit_amount_cents": 200  }
    ]
  }'
```

→ `"total_cents": 5600`, computed from 5000 + (3 × 200).

### 3. Pay it — success

```bash
curl -s -X POST localhost:8080/v1/invoices/inv_REPLACE_ME/pay \
  -H "Authorization: Bearer $KEY" \
  -H "Idempotency-Key: $(uuidgen)" \
  -H "Content-Type: application/json" \
  -d '{"card_token":"tok_success"}'
```

→ `200`, attempt `succeeded`, invoice `paid`. Watch `docker compose logs -f
webhook-receiver` for `invoice.paid` arriving with `signature: ✓ VALID`.

### 4. Pay it — failure

```bash
curl -s -X POST localhost:8080/v1/invoices/inv_REPLACE_ME/pay \
  -H "Authorization: Bearer $KEY" \
  -H "Idempotency-Key: $(uuidgen)" \
  -H "Content-Type: application/json" \
  -d '{"card_token":"tok_card_declined"}'
```

→ `200` with `"status": "failed"`, and the invoice stays `open`.

A declined card is **not** an HTTP error: the request succeeded and produced a
definitive answer. The HTTP status describes the API call; the `status` field
describes the money.

### Card tokens

| Token | Behaviour |
|---|---|
| `tok_success` | succeeds after ~100ms |
| `tok_insufficient_funds` | declined |
| `tok_card_declined` | declined |
| `tok_timeout` | processor sleeps 30s, then **succeeds** |
| `tok_network_error` | processor returns 500 |

---

## The interesting bits

### Two people paying at once

```bash
INV=inv_REPLACE_ME
for i in $(seq 1 10); do
  (curl -s -o /dev/null -w "%{http_code} " \
     -X POST "localhost:8080/v1/invoices/$INV/pay" \
     -H "Authorization: Bearer $KEY" \
     -H "Idempotency-Key: race-$i" \
     -H "Content-Type: application/json" \
     -d '{"card_token":"tok_success"}') &
done; wait; echo
```

One `200`, nine `409`. And in `docker compose logs mock-psp`, **exactly one**
`charge resolved` — the API rejecting nine requests is only half the story;
what matters is that the card was charged once.

### The processor goes quiet

```bash
curl -s -X POST localhost:8080/v1/invoices/inv_REPLACE_ME/pay \
  -H "Authorization: Bearer $KEY" \
  -H "Idempotency-Key: $(uuidgen)" \
  -H "Content-Type: application/json" \
  -d '{"card_token":"tok_timeout"}'
```

Hangs 5 seconds, then returns **`202 Accepted`** with `"status": "pending"`.
The card may or may not have been charged, so the invoice is **locked** —
another `/pay` gets `409 payment_already_in_progress`.

About 30 seconds later the processor finishes and charges the card. Roughly ten
seconds after that a background reconciler asks what happened, gets
`succeeded`, and marks the invoice paid — with nobody touching anything.

```bash
docker compose logs -f invoice-service
```

### Tenant isolation

Atlas's key on one of Nova's invoices returns **404, not 403**. A 403 would
confirm the record exists, which turns the API into an oracle for enumerating
another business's data.

### Webhook signatures are really checked

```bash
curl -s -X POST localhost:9000/hooks \
  -H 'Dodo-Signature: t=1758204300,v1=deadbeef' \
  -H 'Content-Type: application/json' \
  -d '{"id":"evt_fake","type":"invoice.paid","data":{"invoice":{"status":"paid"}}}'
```

→ `✗ INVALID → 400 rejected` in the receiver log.

---

## Tests

Three scenarios, as required. Each test creates its own throwaway database and
its own stub processor on a random port, so `cargo test` needs nothing running
except Postgres — no `docker compose` first.

```bash
DATABASE_URL=postgres://postgres:postgres@localhost:5433/invoice_payments \
  cargo test -p invoice-service
```

| File | Asserts |
|---|---|
| `tests/concurrency.rs` | 20 simultaneous `/pay` calls → exactly one 200, and **the processor was called once** |
| `tests/idempotency.rs` | same key twice → byte-identical response, no second charge; same key + different body → 422 |
| `tests/psp_failure.rs` | unknown outcome locks the invoice, then reconciles correctly in **both** directions — charge went through → `paid`; never arrived → payable again |

The stub processor counts calls. That count is the assertion that actually
matters: returning the right status codes while charging someone twice would
pass a weaker test.

---

## Running locally without Docker

Start just the database, then run the three services from source:

```bash
docker compose up postgres     # :5433
```

```bash
cargo run -p mock-psp          # :9100
cargo run -p webhook-receiver  # :9000
cp .env.example .env && cargo run
```

---

## Notes

**Migrations run at startup.** `docker compose up` has to work with no manual
steps, so the binary embeds its migrations and applies them on boot. In
production I'd split that into a separate deploy step — one instance owning
schema changes is easier to observe and roll back than every replica racing to
apply them.

**`.sqlx/` is committed.** SQLx checks every query against a live database
while compiling. There's no database during a Docker build, so the cached query
metadata is used instead. After changing any SQL, regenerate it:

```bash
cargo sqlx prepare --workspace
```

**The mock processor stores charges in memory.** A restart loses them, which
would make a lookup wrongly report that no charge occurred. A real processor is
durable — this is a mock limitation, not a design position.

**`webhook-receiver` is demo infrastructure**, not part of the service. It
exists so webhook delivery, signature verification, retry behaviour and
response-path decoupling can be seen rather than taken on trust.
