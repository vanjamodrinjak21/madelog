# pop (CLI)

The Rust crate behind the `pop` command. It records commits, AI sessions and decisions into an
append-only hash chain in `.pop/log.db`, and renders the project page that `pop publish` sends
to the server.

Full documentation, including installation, every command and the configuration reference, is at
<https://pop.dev/docs/>.

```sh
cargo install --path .     # build and install the binary
pop onboard                # what pop records, what is missing, and set up this repo
```

Development:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Layout: `src/commands/` one module per command, `src/event.rs` and `src/chain.rs` the event model
and the hash chain, `src/store.rs` SQLite, `src/gitrepo.rs` and `src/gitai.rs` the git side,
`src/page/` the Tera renderer, `tests/` integration tests against temporary repositories.

Design spec: `../docs/superpowers/specs/2026-09-17-pop-v1-design.md` section 4. MIT licensed, see
`../LICENSE`.
