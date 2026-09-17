# popserver

The Go service behind `pop publish`. It stores published project pages, keeps a
history of hash-chain roots, serves the public page and its verification record,
and files Lemon Squeezy webhooks for later. Nothing else.

Spec: `docs/superpowers/specs/2026-09-17-pop-v1-design.md` section 5.

```
cmd/popserver        main: config, migrations, HTTP server, graceful shutdown
internal/config      environment, validated once at startup
internal/httpapi     chi router, middleware, every handler
internal/sign        the exact byte strings the CLI signs, plus verification
internal/slug        <adjective>-<noun>-<4 base32> generation and validation
internal/store       Store interface, Postgres implementation, in-memory fake
internal/migrate     tiny embedded migrator (no external tool)
internal/db          sqlc output - generated, committed, never edited by hand
db/migrations        the schema, append-only
db/queries           the SQL sqlc turns into internal/db
```

## Signature scheme (read this first if you are writing the client)

Keys are ed25519. On the wire everything is **lowercase hex**:

| field | bytes | hex characters |
|---|---|---|
| `public_key` | 32 | 64 |
| `signature` | 64 | 128 |
| `root` | 32 (sha256) | 64 |

Signatures are verified on the **exact bytes below**, built from the strings in
the request body as sent. Do not re-serialise, re-format or re-case anything
before signing: the server signs over what it received.

### Publish — `POST /v1/pages`

```
root + "\n" + published_ts + "\n" + slug
```

- `root` is the chain root as lowercase hex sha256, 64 characters.
- `published_ts` is RFC 3339 in UTC, for example `2026-09-17T14:05:00Z`. The
  same string must appear in the request body.
- `slug` is the slug as sent. **When the client has no slug yet it omits the
  field and signs over the empty string**, so the message ends with a trailing
  newline.

First publish, no slug yet:

```
3d9f1c0a...eeff0\n2026-09-17T14:05:00Z\n
```

Re-publishing an existing page:

```
3d9f1c0a...eeff0\n2026-09-17T14:05:00Z\nquiet-heron-k7m2
```

In Rust:

```rust
let msg = format!("{root}\n{published_ts}\n{slug}");   // slug is "" on first publish
let signature = hex::encode(signing_key.sign(msg.as_bytes()).to_bytes());
```

### Delete — `DELETE /v1/pages/{slug}`

```
"delete" + "\n" + slug
```

The slug here is the one in the URL path, never empty:

```
delete\nquiet-heron-k7m2
```

### Lemon Squeezy webhook — `POST /v1/webhooks/lemonsqueezy`

Not ed25519. The `X-Signature` header is a lowercase hex HMAC-SHA256 over the
**raw request body**, keyed with `LEMONSQUEEZY_SECRET`, compared in constant
time. If the secret is empty the endpoint answers `503`, because a webhook that
cannot be verified must not be accepted.

## Endpoints

| Method | Path | Body | Result |
|---|---|---|---|
| POST | `/v1/devices` | `{public_key}` | `{device_id}` — idempotent on the public key |
| POST | `/v1/pages` | `{device_id, slug?, html, root, first_ts, last_ts, published_ts, signature}` | `{slug, url, verify_url, received_at}` |
| GET | `/p/{slug}` | — | the stored html |
| GET | `/v1/verify/{slug}` | — | `{slug, root, first_ts, last_ts, received_at, device_id}` |
| DELETE | `/v1/pages/{slug}` | `{device_id, signature}` | `{slug, deleted}` |
| POST | `/v1/webhooks/lemonsqueezy` | raw JSON | `{ok}` |
| GET | `/healthz` | — | `{status}`; 503 when the database is unreachable |

Errors are JSON: `{"error": "<code>", "message": "<human readable>"}`.

### Rules the handlers enforce

- **Size.** `html` is at most 2 MB; anything larger is `413`. An absurdly large
  request body is cut off and also answers `413`.
- **Timestamps.** `first_ts`, `last_ts` and `published_ts` are RFC 3339 with a
  zero UTC offset. `received_at` is set by the server and never read from the
  request.
- **Slugs.** Minted by the server as `<adjective>-<noun>-<4 characters of
  lowercase base32>`, for example `quiet-heron-k7m2`. A client may send one back
  to re-publish. Accepted slugs are 3 to 64 characters of lowercase letters,
  digits and single hyphens.
- **Ownership.** Re-publishing the same device and slug overwrites `html`,
  `root` and the timestamps, and appends a new `roots` row. Publishing to a slug
  owned by another device is `403`. Deleted slugs stay reserved and are never
  handed out again.
- **Delete** clears the html and stamps `deleted_at`. The `root` column and the
  whole `roots` history survive. `/p/{slug}` and `/v1/verify/{slug}` then answer
  `404`. Deleting twice is fine. Re-publishing the slug brings the page back.

### Status codes for `POST /v1/pages`

| code | when |
|---|---|
| 200 | stored |
| 400 | malformed body, bad root, bad timestamp, bad slug shape, unparseable signature |
| 401 | the signature does not verify for that device's key |
| 403 | the slug belongs to another device |
| 404 | unknown `device_id` — register it with `POST /v1/devices` first |
| 413 | `html` over 2 MB, or the request body is far too large |

## Configuration

Environment only; see `.env.example`.

| variable | required | default | meaning |
|---|---|---|---|
| `DATABASE_URL` | yes | — | pgx DSN |
| `PUBLIC_BASE_URL` | yes | — | origin used to build returned URLs; trailing slash trimmed |
| `LISTEN_ADDR` | no | `:8080` | bind address |
| `LEMONSQUEEZY_SECRET` | no | empty | empty makes the webhook answer 503 |
| `LOG_LEVEL` | no | `info` | debug, info, warn, error |
| `LOG_FORMAT` | no | `json` | json or text |

## Schema

`devices`, `pages`, `roots`, `billing_events`, plus `schema_migrations` that the
migrator maintains. The migrator embeds `db/migrations/*.sql`, takes a Postgres
advisory lock so two instances cannot race at startup, and applies each
unapplied file in its own transaction. There is no external migration tool and
no down migrations: to change the schema, add `0002_*.sql`. Never edit a file
that has already shipped.

Regenerate the query layer after touching `db/queries` or `db/migrations`:

```sh
make sqlc
```

`internal/db` is generated and committed. Do not edit it by hand.

## Running

```sh
cp .env.example .env
make db-up                  # postgres:16 via docker compose
make run                    # applies migrations, then serves
```

Or everything in containers:

```sh
make up                     # http://localhost:8080
make down
```

## Tests

```sh
make test                   # handler and unit tests; DB tests skip
make test-db                # starts postgres, runs the same suite against it
make lint                   # gofmt check plus go vet
```

Handler tests run against the in-memory store with a real ed25519 key generated
in the test, so the signature path is exercised for real. The in-memory fake and
the Postgres store run the **same** conformance suite
(`internal/store/storetest`), so the fake used by the handler tests cannot drift
from the database.

Database tests run only when `DATABASE_URL` is set and skip otherwise:

```sh
DATABASE_URL='postgres://pop:pop@localhost:5432/pop?sslmode=disable' go test ./...
```

## Privacy

The server stores the rendered html, the root, the signature timestamps and the
device's public key. That is all `pop publish` sends (spec section 7). There is
no account, no email on the publish path, and `DELETE /v1/pages/{slug}` works
from day one. Billing payloads are stored verbatim, and only after their HMAC
has been verified.
