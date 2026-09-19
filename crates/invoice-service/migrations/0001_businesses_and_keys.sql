-- A business is our customer. Everything else hangs off one.
CREATE TABLE businesses (
    id          TEXT PRIMARY KEY,
    name        TEXT        NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Many keys per business ON PURPOSE: that is the whole rotation story.
-- Issue a new key, deploy it, revoke the old one. No downtime, no feature.
CREATE TABLE api_keys (
    id          TEXT PRIMARY KEY,
    business_id TEXT        NOT NULL REFERENCES businesses (id),
    key_hash    TEXT        NOT NULL UNIQUE,  -- sha256(plaintext). we never store the key.
    key_last4   TEXT        NOT NULL,         -- for logs/display only, never for auth
    revoked_at  TIMESTAMPTZ,                  -- soft delete: keep the audit trail
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- UNIQUE on key_hash already gives us the index auth looks up by.
CREATE INDEX api_keys_business_idx ON api_keys (business_id);
