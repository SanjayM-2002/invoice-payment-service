-- A payment attempt is the paper trail AND the lock. One row, two jobs.
CREATE TABLE payment_attempts (
    id           TEXT PRIMARY KEY,   -- also handed to the PSP as ITS idempotency key
    invoice_id   TEXT NOT NULL REFERENCES invoices (id),

    status       TEXT NOT NULL DEFAULT 'pending'
                 CHECK (status IN ('pending', 'succeeded', 'failed')),

    amount_cents BIGINT NOT NULL,    -- snapshot of what we asked the PSP to charge
    psp_ref      TEXT,
    failure_code TEXT,

    -- Reconciliation bookkeeping. next_reconcile_at is set AT INSERT, never in
    -- an error handler: if the process dies mid-charge, a NULL here would make
    -- the row invisible to the worker forever and brick the invoice.
    reconcile_attempts INTEGER     NOT NULL DEFAULT 0,
    next_reconcile_at  TIMESTAMPTZ,
    needs_review       BOOLEAN     NOT NULL DEFAULT FALSE,

    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    resolved_at  TIMESTAMPTZ

    -- We deliberately do NOT store the card token. Reconciliation ASKS the PSP
    -- what happened rather than re-sending the charge, so the token is never
    -- needed again. Don't keep what you don't need.
);

-- ┌────────────────────────────────────────────────────────────────┐
-- │ THE concurrency mechanism.                                     │
-- │ At most one pending attempt per invoice, ever. Two simultaneous │
-- │ /pay calls both try to INSERT; Postgres lets exactly one        │
-- │ through and the loser gets a unique violation -> 409.           │
-- │ Partial, so finished attempts don't occupy it.                  │
-- └────────────────────────────────────────────────────────────────┘
CREATE UNIQUE INDEX one_inflight_attempt_per_invoice
    ON payment_attempts (invoice_id)
    WHERE status = 'pending';

-- The reconciler's work queue. Also partial: ~all attempts end terminal, so
-- this index only ever holds in-flight rows and stays tiny forever.
CREATE INDEX payment_attempts_due_idx
    ON payment_attempts (next_reconcile_at)
    WHERE status = 'pending' AND needs_review = FALSE;

CREATE INDEX payment_attempts_invoice_idx ON payment_attempts (invoice_id);

-- Guards the OTHER kind of duplicate: the same request arriving twice,
-- rather than two different requests at the same instant.
CREATE TABLE idempotency_keys (
    business_id   TEXT        NOT NULL REFERENCES businesses (id),
    key           TEXT        NOT NULL,
    request_hash  TEXT        NOT NULL,   -- sha256(path + body); catches key reuse
    status        TEXT        NOT NULL CHECK (status IN ('in_progress', 'completed')),
    response_code INTEGER,
    response_body JSONB,
    attempt_id    TEXT        REFERENCES payment_attempts (id),
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at    TIMESTAMPTZ NOT NULL,

    -- Composite key: business A's "key-1" must never collide with business
    -- B's. Without business_id here, a replay would leak across tenants.
    PRIMARY KEY (business_id, key)
);

CREATE INDEX idempotency_keys_expires_idx ON idempotency_keys (expires_at);
