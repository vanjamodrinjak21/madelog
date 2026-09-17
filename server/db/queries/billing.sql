-- name: InsertBillingEvent :exec
-- The raw, already HMAC-verified webhook body, stored verbatim.
INSERT INTO billing_events (payload)
VALUES ($1);
