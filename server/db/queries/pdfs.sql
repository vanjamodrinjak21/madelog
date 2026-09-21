-- name: GetPagePDF :one
SELECT slug, root, stamp_day, pdf, rendered_at
FROM page_pdfs
WHERE slug = $1 AND root = $2 AND stamp_day = $3;

-- name: UpsertPagePDF :exec
INSERT INTO page_pdfs (slug, root, stamp_day, pdf)
VALUES ($1, $2, $3, $4)
ON CONFLICT (slug, root, stamp_day) DO UPDATE
    SET pdf = EXCLUDED.pdf,
        rendered_at = now();

-- name: DeleteStalePagePDFs :exec
-- Everything rendered for a root the page no longer has.
DELETE FROM page_pdfs
WHERE slug = $1 AND root <> $2;
