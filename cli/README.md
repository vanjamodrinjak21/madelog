# madelog (CLI)

The Rust crate behind the `madelog` command. It records commits, AI sessions and decisions into an
append-only hash chain in `.madelog/log.db`, and renders the project page that `madelog publish` sends
to the server.

Full documentation, including installation, every command and the configuration reference, is at
<https://madelog.app/docs/>.

```sh
cargo install --path .     # build and install the binary
madelog onboard                # what madelog records, what is missing, and set up this repo
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

Design spec: `../docs/superpowers/specs/2026-09-17-madelog-v1-design.md` section 4. MIT licensed, see
`../LICENSE`.
