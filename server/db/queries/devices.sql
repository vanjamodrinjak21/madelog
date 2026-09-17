-- name: UpsertDevice :one
-- Idempotent on public_key: the DO UPDATE is a no-op write that lets us use
-- RETURNING to get the existing row back in one round trip.
INSERT INTO devices (public_key)
VALUES ($1)
ON CONFLICT (public_key) DO UPDATE SET public_key = EXCLUDED.public_key
RETURNING id, public_key, email, created_at;

-- name: GetDevice :one
SELECT id, public_key, email, created_at
FROM devices
WHERE id = $1;
