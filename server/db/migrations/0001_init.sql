-- 0001_init: devices, pages, the root history and the billing event log.
-- Applied at startup by internal/migrate. Migrations are append-only: to change
-- something, add 0002_*.sql. Never edit a file that has already shipped.

CREATE TABLE devices (
    id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    public_key bytea       NOT NULL UNIQUE,
    email      text,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE pages (
    slug         text PRIMARY KEY,
    device_id    uuid        NOT NULL REFERENCES devices (id),
    html         text        NOT NULL,
    root         text        NOT NULL,
    first_ts     timestamptz NOT NULL,
    last_ts      timestamptz NOT NULL,
    published_ts timestamptz NOT NULL,
    received_at  timestamptz NOT NULL DEFAULT now(),
    -- A deleted page keeps its row so the slug is never reused and the root
    -- history stays attached; only the html goes away.
    deleted_at   timestamptz
);

CREATE INDEX pages_device_id_idx ON pages (device_id);

CREATE TABLE roots (
    id          bigserial PRIMARY KEY,
    slug        text        NOT NULL REFERENCES pages (slug),
    root        text        NOT NULL,
    received_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX roots_slug_idx ON roots (slug, id);

CREATE TABLE billing_events (
    id          bigserial PRIMARY KEY,
    received_at timestamptz NOT NULL DEFAULT now(),
    payload     jsonb       NOT NULL
);
