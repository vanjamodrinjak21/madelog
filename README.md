# madelog

madelog runs alongside git and records how a project came together: the commits, the AI sessions,
the one-sentence decisions you write down, and the patterns between them. Every record is
hashed into an append-only chain in `.madelog/log.db`, so the history can be verified instead of
claimed. `madelog publish` turns that log into one static page with a verification badge, which you
send to a company in place of a portfolio.

Documentation: <https://madelog.app/docs/>

## Install

System-wide, the default:

```sh
curl -fsSL https://madelog.app/install.sh | sh
```

```powershell
irm https://madelog.app/install.ps1 | iex
```

Per project, which puts the binary in `<repo>/.madelog/bin/madelog` and leaves your PATH alone:

```sh
curl -fsSL https://madelog.app/install.sh | sh -s -- --project
```

```powershell
& ([scriptblock]::Create((irm https://madelog.app/install.ps1))) -Project
```

Both modes end by running `madelog onboard`, which explains what madelog records, checks for git,
Git AI and Ollama, and offers to set up the repository you are in.

Until the first release is tagged, build from source:

```sh
git clone https://github.com/vanjamodrinjak21/madelog
cd madelog
cargo install --path cli
madelog onboard
```

## The three commands you use

| Command | What it does |
|---|---|
| `madelog init` | Asks what you are building, for whom, and which AI tools you use. Creates `.madelog/`, installs the post-commit hook, writes the first event. |
| `madelog note "<sentence>"` | Records one decision against the current commit. Add `--private` to keep it out of the page. |
| `madelog publish` | Runs the analysis, opens the preview, renders the page, and publishes it. `--local` stops at `.madelog/site/index.html`. |

Everything else (`madelog log`, `madelog verify`, `madelog export`, `madelog hide`, `madelog config`, `madelog analyze`,
`madelog summarize`) exists for when you need it. Nothing leaves your machine until you run
`madelog publish` without `--local`.

## Repository layout

```
cli/        Rust CLI `madelog`
server/     Go API (`madelog-server`)
schema/     event JSON Schema shared by both
packaging/  Homebrew formula and winget manifest templates
.github/    CI and release workflows
```

The website, design assets and internal documents are maintained outside this repository.

## Development

```sh
# CLI
cd cli
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test

# Server (database tests skip unless DATABASE_URL is set)
cd server
make lint
go test ./...
```

`server/Makefile` also has `make db-up` and `make test-db` to run the Go suite against a local
Postgres in Docker. CI runs both suites on every push and pull request.

Contributions: see [CONTRIBUTING.md](CONTRIBUTING.md).

## License

MIT. See [LICENSE](LICENSE).
