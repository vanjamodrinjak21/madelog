# pop

pop runs alongside git and records how a project came together: the commits, the AI sessions,
the one-sentence decisions you write down, and the patterns between them. Every record is
hashed into an append-only chain in `.pop/log.db`, so the history can be verified instead of
claimed. `pop publish` turns that log into one static page with a verification badge, which you
send to a company in place of a portfolio.

Documentation: <https://pop.dev/docs/>

## Install

System-wide, the default:

```sh
curl -fsSL https://pop.dev/install.sh | sh
```

```powershell
irm https://pop.dev/install.ps1 | iex
```

Per project, which puts the binary in `<repo>/.pop/bin/pop` and leaves your PATH alone:

```sh
curl -fsSL https://pop.dev/install.sh | sh -s -- --project
```

```powershell
& ([scriptblock]::Create((irm https://pop.dev/install.ps1))) -Project
```

Both modes end by running `pop onboard`, which explains what pop records, checks for git,
Git AI and Ollama, and offers to set up the repository you are in.

Until the first release is tagged, build from source:

```sh
git clone https://github.com/vanjamodrinjak/pop
cd pop
cargo install --path cli
pop onboard
```

## The three commands you use

| Command | What it does |
|---|---|
| `pop init` | Asks what you are building, for whom, and which AI tools you use. Creates `.pop/`, installs the post-commit hook, writes the first event. |
| `pop note "<sentence>"` | Records one decision against the current commit. Add `--private` to keep it out of the page. |
| `pop publish` | Runs the analysis, opens the preview, renders the page, and publishes it. `--local` stops at `.pop/site/index.html`. |

Everything else (`pop log`, `pop verify`, `pop export`, `pop hide`, `pop config`, `pop analyze`,
`pop summarize`) exists for when you need it. Nothing leaves your machine until you run
`pop publish` without `--local`.

## Repository layout

```
cli/        Rust crate `pop`: the binary and the library the tests drive
server/     Go service `popserver`: stores published pages, roots and signatures
landing/    Next.js site for pop.dev, statically exported, including the docs at /docs/
schema/     event.schema.json, the event model both the CLI and the server validate against
docs/       specs and implementation plans
packaging/  Homebrew formula and winget manifest templates
```

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

# Landing and docs
cd landing
pnpm install
pnpm lint
pnpm build
```

`server/Makefile` also has `make db-up` and `make test-db` to run the Go suite against a local
Postgres in Docker. CI runs all three suites on every push and pull request.

Contributions: see [CONTRIBUTING.md](CONTRIBUTING.md).

## License

MIT. See [LICENSE](LICENSE).
