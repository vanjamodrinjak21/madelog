-- 0002_plans_and_pdfs: a plan per device, and the PDFs rendered for Pro pages.
--
-- plan is 'free' or 'pro'. plan_source says who set it ('manual', or
-- 'lemonsqueezy:<order or license id>' once activation is wired); plan_until
-- is NULL for no end. A PDF is cached per (slug, root, day the stamp names),
-- so a re-publish, which changes the root, renders afresh, and the exported
-- date on the stamp is never more than a day stale.

ALTER TABLE devices ADD COLUMN plan        text NOT NULL DEFAULT 'free';
ALTER TABLE devices ADD COLUMN plan_source text;
ALTER TABLE devices ADD COLUMN plan_until  timestamptz;

CREATE TABLE page_pdfs (
    slug        text        NOT NULL REFERENCES pages (slug),
    root        text        NOT NULL,
    stamp_day   text        NOT NULL,
    pdf         bytea       NOT NULL,
    rendered_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (slug, root, stamp_day)
);
