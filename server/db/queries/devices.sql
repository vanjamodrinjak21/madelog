-- name: UpsertDevice :one
-- Idempotent on public_key: the DO UPDATE is a no-op write that lets us use
-- RETURNING to get the existing row back in one round trip.
INSERT INTO devices (public_key)
VALUES ($1)
ON CONFLICT (public_key) DO UPDATE SET public_key = EXCLUDED.public_key
RETURNING id, public_key, email, created_at, plan, plan_source, plan_until;

-- name: GetDevice :one
SELECT id, public_key, email, created_at, plan, plan_source, plan_until
FROM devices
WHERE id = $1;

-- name: SetDevicePlan :one
UPDATE devices
SET plan = $2, plan_source = $3, plan_until = $4
WHERE id = $1
RETURNING id, public_key, email, created_at, plan, plan_source, plan_until;
