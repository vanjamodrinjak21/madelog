-- name: GetPage :one
-- Returns deleted pages too; the caller decides what that means.
SELECT slug, device_id, html, root, first_ts, last_ts, published_ts, received_at, deleted_at
FROM pages
WHERE slug = $1;

-- name: UpsertPage :one
-- A re-publish overwrites html, root and the timestamps, stamps a fresh
-- received_at, and clears deleted_at so a deleted page can come back.
INSERT INTO pages (slug, device_id, html, root, first_ts, last_ts, published_ts, received_at, deleted_at)
VALUES ($1, $2, $3, $4, $5, $6, $7, now(), NULL)
ON CONFLICT (slug) DO UPDATE
    SET html         = EXCLUDED.html,
        root         = EXCLUDED.root,
        first_ts     = EXCLUDED.first_ts,
        last_ts      = EXCLUDED.last_ts,
        published_ts = EXCLUDED.published_ts,
        received_at  = now(),
        deleted_at   = NULL
RETURNING slug, device_id, html, root, first_ts, last_ts, published_ts, received_at, deleted_at;

-- name: SoftDeletePage :one
-- Idempotent: deleting twice keeps the first deleted_at.
UPDATE pages
SET html       = '',
    deleted_at = COALESCE(deleted_at, now())
WHERE slug = $1
RETURNING slug, device_id, html, root, first_ts, last_ts, published_ts, received_at, deleted_at;

-- name: InsertRoot :exec
INSERT INTO roots (slug, root)
VALUES ($1, $2);

-- name: ListRoots :many
SELECT id, slug, root, received_at
FROM roots
WHERE slug = $1
ORDER BY id;
