# Contributing

Thanks for taking the time. This is a monorepo with three parts: the Rust CLI in `cli/`, the Go
server in `server/`, and the Next.js landing and docs in `landing/`. The event model in
`schema/event.schema.json` is shared, so a change there touches both the CLI and the server.

## Branches

Branch off `main` with one of three prefixes:

- `feature/` for new behaviour
- `fix/` for bug fixes
- `chore/` for build, CI, dependencies and housekeeping

Example: `feature/summary-editing`. Never force-push to `main`.

## Commits

Conventional commits, one concern per commit:

```
feat: add pop summary edit
fix: keep the post-commit hook silent when Git AI notes are missing
chore: bump rusqlite to 0.32
docs: describe the verification badge
refactor: extract the week bucketing
test: cover the reverted-commit pattern
```

The subject is imperative, lower case, no trailing period. Use a body when the reason is not
obvious from the diff. One concern per pull request, unless bundling was asked for.

## Before you open a pull request

Run all three suites that CI runs:

```sh
# Rust
cd cli
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test

# Go (database tests skip unless DATABASE_URL is set; `make test-db` runs them)
cd server
make lint
go test ./...

# Landing and docs
cd landing
pnpm install --frozen-lockfile
pnpm lint
pnpm exec tsc --noEmit
pnpm build
```

New behaviour comes with a test. Rust integration tests live in `cli/tests/` and drive the
library API against a temporary git repository; Go handler tests use `httptest`.

## User-facing text

Every string the CLI, the project page and the default landing print is English. Short
sentences, second person, no emoji, no marketing adjectives. Croatian and German exist only as
landing translations.

## Secrets

Never commit a `.env` file, an API key, a token or a device key. `.env` and `.env.*` are in
`.gitignore`; keep them there. `server/.env.example` documents every variable the server reads,
with empty values. Deployment secrets live in the hosting provider, not in the repository.

## Privacy rules that are not negotiable

pop records only what the spec says it records: git history, Git AI notes, and what you type
into `pop note`. No keystrokes, no screenshots, no ignored or untracked files, nothing outside
the repository. If a change would send anything to the network outside of `pop publish`, it does
not belong in this project.
