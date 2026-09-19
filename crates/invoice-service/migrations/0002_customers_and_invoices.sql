CREATE TABLE customers (
    id          TEXT PRIMARY KEY,
    business_id TEXT        NOT NULL REFERENCES businesses (id),
    name        TEXT        NOT NULL,
    email       TEXT        NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX customers_business_created_idx
    ON customers (business_id, created_at DESC);

CREATE TABLE invoices (
    id          TEXT PRIMARY KEY,
    -- business_id is denormalised (reachable via customer) so that every
    -- tenant-scoped query can filter without a join. The bug that leaks data
    -- in multi-tenant systems is the query that forgot the filter.
    business_id TEXT        NOT NULL REFERENCES businesses (id),
    customer_id TEXT        NOT NULL REFERENCES customers (id),

    status      TEXT        NOT NULL DEFAULT 'open'
                CHECK (status IN ('open', 'paid', 'void', 'uncollectible')),

    -- Integer minor units. No floats anywhere in the money path.
    total_cents BIGINT      NOT NULL CHECK (total_cents > 0),

    -- Single currency, enforced rather than assumed. An integer amount is
    -- meaningless without one: 1999 is $19.99 but also ¥1999 and 1.999 KWD.
    currency    CHAR(3)     NOT NULL DEFAULT 'USD' CHECK (currency = 'USD'),

    due_date    TIMESTAMPTZ NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Serves "list my invoices" and "list my open invoices" both.
CREATE INDEX invoices_business_status_created_idx
    ON invoices (business_id, status, created_at DESC);

CREATE INDEX invoices_customer_idx ON invoices (customer_id);

CREATE TABLE line_items (
    id                TEXT    PRIMARY KEY,
    invoice_id        TEXT    NOT NULL REFERENCES invoices (id) ON DELETE CASCADE,
    description       TEXT    NOT NULL CHECK (length(description) BETWEEN 1 AND 500),
    -- The bounds live here, not only in the handler. These are exactly the
    -- fields we said we would never trust from a client.
    quantity          INTEGER NOT NULL CHECK (quantity BETWEEN 1 AND 10000),
    unit_amount_cents BIGINT  NOT NULL CHECK (unit_amount_cents BETWEEN 0 AND 100000000),
    sort_order        INTEGER NOT NULL
    -- no line_total column: it is derivable, and storing it invites drift
);

CREATE INDEX line_items_invoice_idx ON line_items (invoice_id);
