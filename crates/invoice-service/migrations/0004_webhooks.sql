CREATE TABLE webhook_endpoints (
    id          TEXT        PRIMARY KEY,
    business_id TEXT        NOT NULL REFERENCES businesses (id),
    url         TEXT        NOT NULL,
    -- Stored in the clear, unlike api_keys.key_hash. We must SIGN with this,
    -- and you cannot sign with a hash. Would be encrypted at rest in prod.
    secret      TEXT        NOT NULL,
    active      BOOLEAN     NOT NULL DEFAULT TRUE,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX webhook_endpoints_business_idx
    ON webhook_endpoints (business_id) WHERE active;

-- An EVENT is an immutable fact: "this happened". Written in the same
-- transaction as the state change it describes -- that's the outbox.
CREATE TABLE webhook_events (
    id          TEXT        PRIMARY KEY,   -- evt_... the receiver's dedup key
    business_id TEXT        NOT NULL REFERENCES businesses (id),
    type        TEXT        NOT NULL,
    payload     JSONB       NOT NULL,      -- snapshot at emit time, not delivery time
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX webhook_events_business_created_idx
    ON webhook_events (business_id, created_at DESC);

-- A DELIVERY is a transport attempt: mutable, retried, gives up.
-- Split from the event so evt_... stays stable across all 8 tries and across
-- multiple endpoints.
CREATE TABLE webhook_deliveries (
    id               TEXT        PRIMARY KEY,
    event_id         TEXT        NOT NULL REFERENCES webhook_events (id),
    endpoint_id      TEXT        NOT NULL REFERENCES webhook_endpoints (id),
    status           TEXT        NOT NULL DEFAULT 'pending'
                     CHECK (status IN ('pending', 'delivered', 'exhausted')),
    attempts         INTEGER     NOT NULL DEFAULT 0,
    next_attempt_at  TIMESTAMPTZ,
    last_status_code INTEGER,
    last_error       TEXT,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    delivered_at     TIMESTAMPTZ
    -- 'exhausted' rows are kept forever. "What did we fail to deliver" is
    -- the 2am question.
);

-- Same trick as the reconciler queue: partial, so it holds only live work.
CREATE INDEX webhook_deliveries_due_idx
    ON webhook_deliveries (next_attempt_at) WHERE status = 'pending';

CREATE INDEX webhook_deliveries_event_idx ON webhook_deliveries (event_id);
