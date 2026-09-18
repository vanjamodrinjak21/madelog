//! `madelog publish` and `madelog unpublish` against a real socket.
//!
//! The server here is deliberately small and deliberately strict: it parses
//! the HTTP by hand, rebuilds the byte string from `server/README.md` out of
//! the fields it received, and verifies the ed25519 signature against the
//! public key the device registered. If madelog ever signs the wrong bytes, these
//! tests fail rather than the user finding out at publish time.

mod common;

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use common::*;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use madelog::preview::{Key, Scripted};

// ---------------------------------------------------------------------------
// the fake server
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Seen {
    method: String,
    path: String,
    body: serde_json::Value,
}

#[derive(Default)]
struct State {
    seen: Vec<Seen>,
    /// device_id -> public key, lowercase hex.
    devices: BTreeMap<String, String>,
    /// slug -> (device_id, html, root).
    pages: BTreeMap<String, (String, String, String)>,
    minted: usize,
    /// Answer the next `POST /v1/pages` with 404, as a server with a fresh
    /// database would.
    forget_device_once: bool,
    /// Fall over on the next upload that carries a slug — that is, on the
    /// confirmation that follows a successful first publish.
    break_the_confirmation: bool,
}

struct FakeServer {
    url: String,
    state: Arc<Mutex<State>>,
}

impl FakeServer {
    fn start() -> FakeServer {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let url = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(State::default()));
        let shared = Arc::clone(&state);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                if let Err(err) = handle(stream, &shared) {
                    eprintln!("fake madelog server: {err}");
                }
            }
        });
        FakeServer { url, state }
    }

    fn requests(&self) -> Vec<Seen> {
        self.state.lock().unwrap().seen.clone()
    }

    fn pages(&self) -> BTreeMap<String, (String, String, String)> {
        self.state.lock().unwrap().pages.clone()
    }

    fn forget_the_device_once(&self) {
        self.state.lock().unwrap().forget_device_once = true;
    }

    fn break_the_confirmation(&self) {
        self.state.lock().unwrap().break_the_confirmation = true;
    }

    fn requests_to(&self, path: &str) -> Vec<Seen> {
        self.requests()
            .into_iter()
            .filter(|r| r.path == path)
            .collect()
    }
}

fn handle(mut stream: TcpStream, state: &Mutex<State>) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(());
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("/").to_string();

    let mut length = 0usize;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 || header.trim().is_empty() {
            break;
        }
        if let Some(value) = header.to_ascii_lowercase().strip_prefix("content-length:") {
            length = value.trim().parse().unwrap_or(0);
        }
    }
    let mut raw = vec![0u8; length];
    if length > 0 {
        reader.read_exact(&mut raw)?;
    }
    let text = String::from_utf8_lossy(&raw).into_owned();
    let body: serde_json::Value = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);

    let (status, answer) = {
        let mut state = state.lock().unwrap();
        state.seen.push(Seen {
            method: method.clone(),
            path: path.clone(),
            body: body.clone(),
        });
        route(&mut state, &method, &path, &body)
    };

    write!(
        stream,
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
        answer.len()
    )?;
    stream.flush()
}

fn route(state: &mut State, method: &str, path: &str, body: &serde_json::Value) -> (u16, String) {
    let field = |key: &str| {
        body.get(key)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };

    match (method, path) {
        ("POST", "/v1/devices") => {
            let public_key = field("public_key");
            if public_key.len() != 64 || hex::decode(&public_key).is_err() {
                return (400, error("bad_public_key"));
            }
            // Idempotent on the public key, exactly as the README promises.
            let device_id = format!("dev-{}", &public_key[..12]);
            state.devices.insert(device_id.clone(), public_key);
            (200, format!(r#"{{"device_id":"{device_id}"}}"#))
        }

        ("POST", "/v1/pages") => {
            if std::mem::take(&mut state.forget_device_once) {
                return (404, error("unknown_device"));
            }
            let device_id = field("device_id");
            let Some(public_key) = state.devices.get(&device_id).cloned() else {
                return (404, error("unknown_device"));
            };

            let root = field("root");
            let published_ts = field("published_ts");
            // An absent slug signs as the empty string.
            let slug_sent = body
                .get("slug")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let message = format!("{root}\n{published_ts}\n{slug_sent}");
            if !verifies(&public_key, message.as_bytes(), &field("signature")) {
                return (401, error("bad_signature"));
            }
            if !slug_sent.is_empty() && std::mem::take(&mut state.break_the_confirmation) {
                return (500, error("having_a_moment"));
            }
            if field("html").len() > 2 * 1024 * 1024 {
                return (413, error("too_large"));
            }
            for key in ["first_ts", "last_ts", "published_ts"] {
                let value = field(key);
                if !value.ends_with('Z') || chrono::DateTime::parse_from_rfc3339(&value).is_err() {
                    return (400, error("bad_timestamp"));
                }
            }

            let slug = if slug_sent.is_empty() {
                state.minted += 1;
                format!("quiet-heron-k7m{}", state.minted)
            } else {
                if let Some((owner, _, _)) = state.pages.get(&slug_sent) {
                    if owner != &device_id {
                        return (403, error("not_your_page"));
                    }
                }
                slug_sent
            };
            state
                .pages
                .insert(slug.clone(), (device_id, field("html"), root));
            (
                200,
                serde_json::json!({
                    "slug": slug,
                    "url": format!("https://pages.test/p/{slug}"),
                    "verify_url": format!("https://pages.test/v1/verify/{slug}"),
                    "received_at": "2026-09-18T09:00:00Z",
                })
                .to_string(),
            )
        }

        ("DELETE", _) if path.starts_with("/v1/pages/") => {
            let slug = path.trim_start_matches("/v1/pages/").to_string();
            let device_id = field("device_id");
            let Some(public_key) = state.devices.get(&device_id).cloned() else {
                return (404, error("unknown_device"));
            };
            let message = format!("delete\n{slug}");
            if !verifies(&public_key, message.as_bytes(), &field("signature")) {
                return (401, error("bad_signature"));
            }
            state.pages.remove(&slug);
            (200, format!(r#"{{"slug":"{slug}","deleted":true}}"#))
        }

        _ => (404, error("no_route")),
    }
}

fn error(code: &str) -> String {
    format!(r#"{{"error":"{code}","message":"the fake server said {code}"}}"#)
}

/// Real ed25519, against the key the device actually registered.
fn verifies(public_key_hex: &str, message: &[u8], signature_hex: &str) -> bool {
    let Ok(pk) = hex::decode(public_key_hex) else {
        return false;
    };
    let Ok(pk): Result<[u8; 32], _> = pk.try_into() else {
        return false;
    };
    let Ok(key) = VerifyingKey::from_bytes(&pk) else {
        return false;
    };
    let Ok(sig) = hex::decode(signature_hex) else {
        return false;
    };
    let Ok(sig): Result<[u8; 64], _> = sig.try_into() else {
        return false;
    };
    key.verify(message, &Signature::from_bytes(&sig)).is_ok()
}

// ---------------------------------------------------------------------------
// a repository with something in it
// ---------------------------------------------------------------------------

/// `madelog init`, `commits` commits through the hook, and one decision.
fn repo_with_history(commits: usize) -> TestRepo {
    let repo = init_repo();
    init_madelog(&repo);
    // Keep the tests off the network and off any Ollama this machine may run.
    madelog::run_capture(repo.root(), &["config", "set", "llm.provider", "none"]).unwrap();

    for i in 0..commits {
        repo.write(
            &format!("src/step{i}.rs"),
            &format!("pub fn step{i}() -> usize {{ {i} }}\n"),
        );
        repo.commit_all(&format!("feat: step {i}"));
        madelog::run_capture(repo.root(), &["hook", "post-commit"]).unwrap();
    }
    madelog::run_capture(repo.root(), &["note", "chose sqlite over a file per event"]).unwrap();
    repo
}

fn config_of(repo: &TestRepo) -> madelog::config::Config {
    madelog::config::Config::load(&repo.path(".madelog/config.toml")).unwrap()
}

fn page_html(repo: &TestRepo) -> String {
    std::fs::read_to_string(repo.path(".madelog/site/index.html")).expect("the page was written")
}

fn publish_body(server: &FakeServer, nth: usize) -> serde_json::Value {
    server.requests_to("/v1/pages")[nth].body.clone()
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[test]
fn publish_registers_signs_and_stores_the_slug() {
    let server = FakeServer::start();
    let repo = repo_with_history(3);

    let out = madelog::run_capture(repo.root(), &["publish", "--yes", "--server", &server.url])
        .expect("publish");

    // The device registered first, then the page went up, then the same page
    // went up again with the badge the server's answer made possible.
    let calls: Vec<String> = server
        .requests()
        .iter()
        .map(|r| format!("{} {}", r.method, r.path))
        .collect();
    assert_eq!(
        calls,
        vec!["POST /v1/devices", "POST /v1/pages", "POST /v1/pages"],
        "one registration, one publish, one confirmation"
    );

    // The registration carries the device's public key and nothing else.
    let registration = &server.requests_to("/v1/devices")[0].body;
    let public_key = registration["public_key"].as_str().unwrap();
    assert_eq!(public_key.len(), 64);
    assert_eq!(registration.as_object().unwrap().len(), 1);

    // The publish body is exactly the documented set of fields, with no slug.
    let body = publish_body(&server, 0);
    let mut fields: Vec<&str> = body
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    fields.sort_unstable();
    assert_eq!(
        fields,
        vec![
            "device_id",
            "first_ts",
            "html",
            "last_ts",
            "published_ts",
            "root",
            "signature"
        ]
    );
    assert_eq!(body["root"].as_str().unwrap().len(), 64);
    assert_eq!(body["signature"].as_str().unwrap().len(), 128);
    assert!(body["published_ts"].as_str().unwrap().ends_with('Z'));

    // The signature is over root + "\n" + published_ts + "\n" + "".
    let message = format!(
        "{}\n{}\n",
        body["root"].as_str().unwrap(),
        body["published_ts"].as_str().unwrap()
    );
    assert!(
        verifies(
            public_key,
            message.as_bytes(),
            body["signature"].as_str().unwrap()
        ),
        "the first publish signs the empty slug"
    );

    // The root the server was told is the root of this repository's chain.
    let chain_root = madelog::store::Store::open(&repo.path(".madelog/log.db"))
        .unwrap()
        .root()
        .unwrap();
    assert_eq!(body["root"].as_str().unwrap(), chain_root);

    // The page was rendered twice: the second time with the slug in it.
    let slug = server.pages().keys().next().unwrap().clone();
    let html = page_html(&repo);
    assert!(html.contains(&slug), "the badge carries the slug: {slug}");
    assert!(
        !body["html"].as_str().unwrap().contains(&slug),
        "the first upload was rendered before the slug existed"
    );

    // The confirmation carries the slug, is signed over those very bytes, and
    // is what the server ends up serving at /p/{slug}.
    let confirmation = publish_body(&server, 1);
    assert_eq!(confirmation["slug"].as_str(), Some(slug.as_str()));
    assert!(
        verifies(
            public_key,
            format!(
                "{}\n{}\n{slug}",
                confirmation["root"].as_str().unwrap(),
                confirmation["published_ts"].as_str().unwrap()
            )
            .as_bytes(),
            confirmation["signature"].as_str().unwrap()
        ),
        "the confirmation signs the slug it was just given"
    );
    assert_eq!(
        confirmation["root"].as_str(),
        body["root"].as_str(),
        "the same chain root, because nothing was appended in between"
    );

    let verify_url = format!("https://pages.test/v1/verify/{slug}");
    let stored = server.pages()[&slug].1.clone();
    assert!(
        stored.contains(&verify_url),
        "what /p/{slug} serves carries the confirmed badge, not \"not yet published\""
    );
    assert_eq!(stored, html, "the served page is the page on disk");

    // And the repository remembers who it is and what it owns.
    let config = config_of(&repo);
    assert_eq!(
        config.server.device_id.as_deref(),
        Some(format!("dev-{}", &public_key[..12])).as_deref()
    );
    assert_eq!(config.server.slug.as_deref(), Some(slug.as_str()));

    assert!(out.contains("published"), "{out}");
    assert!(out.contains(&slug), "{out}");
}

#[test]
fn a_second_publish_reuses_the_slug_and_signs_it() {
    let server = FakeServer::start();
    let repo = repo_with_history(3);

    madelog::run_capture(repo.root(), &["publish", "--yes", "--server", &server.url]).unwrap();
    let slug = config_of(&repo).server.slug.unwrap();

    madelog::run_capture(repo.root(), &["publish", "--yes", "--server", &server.url]).unwrap();

    // [0] and [1] are the first publish and its confirmation; [2] is the
    // second publish, which knows the slug before it sends anything.
    assert_eq!(server.requests_to("/v1/pages").len(), 4);
    let second = publish_body(&server, 2);
    assert_eq!(second["slug"].as_str(), Some(slug.as_str()), "same page");
    let message = format!(
        "{}\n{}\n{slug}",
        second["root"].as_str().unwrap(),
        second["published_ts"].as_str().unwrap()
    );
    let public_key = server.requests_to("/v1/devices")[0].body["public_key"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        verifies(
            &public_key,
            message.as_bytes(),
            second["signature"].as_str().unwrap()
        ),
        "re-publishing signs the slug it already owns"
    );

    // One page on the server, not two.
    assert_eq!(server.pages().len(), 1);
    assert_eq!(config_of(&repo).server.slug.as_deref(), Some(slug.as_str()));

    // The device is registered once and then remembered.
    assert_eq!(server.requests_to("/v1/devices").len(), 1);
}

#[test]
fn local_writes_the_page_and_never_touches_the_network() {
    let server = FakeServer::start();
    let repo = repo_with_history(2);

    let out = madelog::run_capture(
        repo.root(),
        &["publish", "--local", "--yes", "--server", &server.url],
    )
    .expect("local publish");

    assert!(repo.path(".madelog/site/index.html").is_file());
    assert!(out.contains("index.html"), "{out}");
    assert!(out.contains("not published"), "{out}");
    assert!(
        server.requests().is_empty(),
        "--local sends nothing: {:?}",
        server.requests()
    );
    assert_eq!(config_of(&repo).server.slug, None);
}

#[test]
fn a_thin_project_is_warned_about_and_published_anyway() {
    let server = FakeServer::start();
    let repo = repo_with_history(1);

    let out = madelog::run_capture(repo.root(), &["publish", "--yes", "--server", &server.url])
        .expect("a thin project still publishes");

    assert!(out.contains("warning:"), "{out}");
    assert!(out.contains("says very little"), "{out}");
    assert!(out.contains("still publish"), "{out}");
    assert_eq!(
        server.requests_to("/v1/pages").len(),
        2,
        "it published, then confirmed"
    );
}

#[test]
fn a_project_with_enough_in_it_is_not_warned_about() {
    let server = FakeServer::start();
    let repo = repo_with_history(16);

    let out = madelog::run_capture(repo.root(), &["publish", "--yes", "--server", &server.url])
        .expect("publish");
    assert!(!out.contains("says very little"), "{out}");
}

#[test]
fn aborting_the_preview_exits_one_and_sends_nothing() {
    let server = FakeServer::start();
    let repo = repo_with_history(3);
    // The hook already rendered a page; an abort must not touch it.
    let page_before = std::fs::read_to_string(repo.path(".madelog/site/index.html")).unwrap();

    let mut out: Vec<u8> = Vec::new();
    let mut input = std::io::empty();
    let mut ctx = madelog::Ctx {
        cwd: repo.root().to_path_buf(),
        input: &mut input,
        out: &mut out,
    };
    let err = madelog::commands::publish::run_with(
        &mut ctx,
        madelog::commands::publish::Options {
            local: false,
            server: Some(server.url.clone()),
            yes: false,
        },
        &mut Scripted::abort(),
    )
    .unwrap_err();
    drop(ctx);

    assert_eq!(exit_code(&err), Some(1));
    assert_eq!(err.to_string(), "publish aborted");
    assert!(
        server.requests().is_empty(),
        "an aborted publish sends nothing: {:?}",
        server.requests()
    );
    assert_eq!(
        std::fs::read_to_string(repo.path(".madelog/site/index.html")).unwrap(),
        page_before,
        "an aborted publish leaves the page exactly as the hook wrote it"
    );
}

#[test]
fn confirming_the_preview_publishes() {
    let server = FakeServer::start();
    let repo = repo_with_history(3);

    let mut out: Vec<u8> = Vec::new();
    let mut input = std::io::empty();
    let mut ctx = madelog::Ctx {
        cwd: repo.root().to_path_buf(),
        input: &mut input,
        out: &mut out,
    };
    madelog::commands::publish::run_with(
        &mut ctx,
        madelog::commands::publish::Options {
            local: false,
            server: Some(server.url.clone()),
            yes: false,
        },
        &mut Scripted::confirm(),
    )
    .expect("confirming publishes");

    assert_eq!(
        server.requests_to("/v1/pages").len(),
        2,
        "the publish and its confirmation"
    );
    assert!(config_of(&repo).server.slug.is_some());
}

#[test]
fn without_a_terminal_the_preview_is_skipped_and_publish_goes_through() {
    let server = FakeServer::start();
    let repo = repo_with_history(3);

    let mut out: Vec<u8> = Vec::new();
    let mut input = std::io::empty();
    let mut ctx = madelog::Ctx {
        cwd: repo.root().to_path_buf(),
        input: &mut input,
        out: &mut out,
    };
    // A screen that cannot open — the same answer the real one gives when
    // stdout is a pipe. It would abort if it ever ran, so reaching the server
    // proves it did not.
    let mut screen = Scripted::abort();
    screen.available = false;
    madelog::commands::publish::run_with(
        &mut ctx,
        madelog::commands::publish::Options {
            local: false,
            server: Some(server.url.clone()),
            yes: false,
        },
        &mut screen,
    )
    .expect("no terminal means no preview, not no publish");

    assert_eq!(
        server.requests_to("/v1/pages").len(),
        2,
        "the publish and its confirmation"
    );
}

#[test]
fn hiding_a_session_in_the_preview_is_recorded_before_the_page_is_rendered() {
    use madelog::event::{AiSessionPayload, EventKind, HidePayload, Payload};

    let server = FakeServer::start();
    let repo = repo_with_history(2);

    // Give the log an AI session to hide.
    let store = madelog::store::Store::open(&repo.path(".madelog/log.db")).unwrap();
    let session = store
        .append_payload(
            None,
            &Payload::AiSession(AiSessionPayload {
                tool: "claude-code".into(),
                model: Some("sonnet".into()),
                session_id: "s-1".into(),
                prompt: Some("write the store".into()),
                response_excerpt: None,
                files: vec![],
                accepted: true,
            }),
        )
        .unwrap();
    drop(store);

    let mut out: Vec<u8> = Vec::new();
    let mut input = std::io::empty();
    let mut ctx = madelog::Ctx {
        cwd: repo.root().to_path_buf(),
        input: &mut input,
        out: &mut out,
    };
    // Walk down to the session (two commits, one decision, then the session)
    // and hide it, then publish.
    madelog::commands::publish::run_with(
        &mut ctx,
        madelog::commands::publish::Options {
            local: true,
            server: None,
            yes: false,
        },
        &mut Scripted::new(&[Key::Down, Key::Down, Key::Down, Key::Hide, Key::Confirm]),
    )
    .expect("publish");

    let hides: Vec<HidePayload> = events(&repo)
        .into_iter()
        .filter(|e| e.kind == EventKind::Hide)
        .map(|e| serde_json::from_value(e.payload).unwrap())
        .collect();
    assert_eq!(hides.len(), 1, "one session hidden");
    assert_eq!(hides[0].target_id, session.id);
    assert_eq!(hides[0].target_kind, "ai_session");
    assert!(
        String::from_utf8_lossy(&out).contains("hidden"),
        "the command says so once the screen is closed: {}",
        String::from_utf8_lossy(&out)
    );
    assert!(server.requests().is_empty(), "--local sends nothing");
}

#[test]
fn a_forgotten_device_is_registered_again_and_the_publish_retried() {
    let server = FakeServer::start();
    let repo = repo_with_history(3);

    // Publish once so the config carries a device id.
    madelog::run_capture(repo.root(), &["publish", "--yes", "--server", &server.url]).unwrap();
    assert!(config_of(&repo).server.device_id.is_some());

    // Now the server forgets it, once.
    server.forget_the_device_once();
    madelog::run_capture(repo.root(), &["publish", "--yes", "--server", &server.url])
        .expect("madelog registers again and retries");

    let calls: Vec<String> = server
        .requests()
        .iter()
        .map(|r| format!("{} {}", r.method, r.path))
        .collect();
    assert_eq!(
        calls,
        vec![
            // The first publish: register, publish, confirm.
            "POST /v1/devices",
            "POST /v1/pages",
            "POST /v1/pages",
            // The second: a 404, then register, publish, confirm.
            "POST /v1/pages",
            "POST /v1/devices",
            "POST /v1/pages",
            "POST /v1/pages",
        ],
        "the 404 is answered by registering again and sending the page once more"
    );
    assert_eq!(server.pages().len(), 1);
}

#[test]
fn a_failed_confirmation_is_a_warning_because_the_page_is_already_online() {
    let server = FakeServer::start();
    let repo = repo_with_history(3);
    server.break_the_confirmation();

    let out = madelog::run_capture(repo.root(), &["publish", "--yes", "--server", &server.url])
        .expect("a broken confirmation does not fail the publish");

    assert!(out.contains("warning:"), "{out}");
    assert!(out.contains("not yet published"), "{out}");
    assert!(out.contains("published  https://pages.test/p/"), "{out}");

    // The slug is still recorded, so the next publish confirms it.
    let slug = config_of(&repo).server.slug.expect("the slug was kept");
    assert!(
        page_html(&repo).contains(&slug),
        "the local page still got the badge"
    );
    assert!(
        !server.pages()[&slug].1.contains(&slug),
        "the server kept the unconfirmed upload, and says so in the warning"
    );
    assert_eq!(server.requests_to("/v1/pages").len(), 2);
}

#[test]
fn publish_records_when_the_page_went_up_so_status_can_call_it_stale() {
    let server = FakeServer::start();
    let repo = repo_with_history(3);
    assert_eq!(config_of(&repo).server.published_at, None);

    madelog::run_capture(repo.root(), &["publish", "--yes", "--server", &server.url]).unwrap();

    let published_at = config_of(&repo)
        .server
        .published_at
        .expect("publish records when it published");
    let at = chrono::DateTime::parse_from_rfc3339(&published_at)
        .unwrap_or_else(|e| panic!("{published_at} is not RFC 3339: {e}"));

    // The confirmed page was uploaded after it was written, so the recorded
    // moment is not older than the file: `madelog status` must not call a
    // freshly published page stale.
    let written = std::fs::metadata(repo.path(".madelog/site/index.html"))
        .and_then(|m| m.modified())
        .unwrap();
    assert!(
        chrono::DateTime::<chrono::Utc>::from(written) <= at.with_timezone(&chrono::Utc),
        "published_at {published_at} is older than the page it sent"
    );

    let json = madelog::run_capture(repo.root(), &["status", "--json"]).unwrap();
    let status: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(status["page"]["newer_than_publish"], false);
    assert_eq!(
        status["published"]["published_at"].as_str(),
        Some(published_at.as_str())
    );
}

#[test]
fn a_confirmation_that_failed_leaves_the_page_on_disk_ahead_of_the_one_online() {
    let server = FakeServer::start();
    let repo = repo_with_history(3);
    server.break_the_confirmation();

    madelog::run_capture(repo.root(), &["publish", "--yes", "--server", &server.url]).unwrap();

    // The badge page was rendered but never accepted, so what is on disk is
    // newer than what visitors see, and status says so.
    let json = madelog::run_capture(repo.root(), &["status", "--json"]).unwrap();
    let status: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(status["page"]["newer_than_publish"], true);
}

#[test]
fn unpublish_forgets_when_the_page_went_up() {
    let server = FakeServer::start();
    let repo = repo_with_history(3);
    madelog::run_capture(repo.root(), &["publish", "--yes", "--server", &server.url]).unwrap();
    assert!(config_of(&repo).server.published_at.is_some());

    madelog::run_capture(repo.root(), &["unpublish", "--server", &server.url]).unwrap();

    assert_eq!(
        config_of(&repo).server.published_at,
        None,
        "nothing is online, so nothing is out of date"
    );
}

#[test]
fn unpublish_sends_a_valid_delete_signature_and_forgets_the_slug() {
    let server = FakeServer::start();
    let repo = repo_with_history(3);

    madelog::run_capture(repo.root(), &["publish", "--yes", "--server", &server.url]).unwrap();
    let slug = config_of(&repo).server.slug.unwrap();
    assert_eq!(server.pages().len(), 1);

    let out = madelog::run_capture(repo.root(), &["unpublish", "--server", &server.url])
        .expect("unpublish");

    let deletes: Vec<Seen> = server
        .requests()
        .into_iter()
        .filter(|r| r.method == "DELETE")
        .collect();
    assert_eq!(deletes.len(), 1);
    assert_eq!(deletes[0].path, format!("/v1/pages/{slug}"));

    let body = &deletes[0].body;
    let mut fields: Vec<&str> = body
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    fields.sort_unstable();
    assert_eq!(fields, vec!["device_id", "signature"]);

    let public_key = server.requests_to("/v1/devices")[0].body["public_key"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        verifies(
            &public_key,
            format!("delete\n{slug}").as_bytes(),
            body["signature"].as_str().unwrap()
        ),
        "the delete signs \"delete\\n\" + slug"
    );

    assert!(server.pages().is_empty(), "the page is gone");
    assert_eq!(config_of(&repo).server.slug, None, "and so is the slug");
    assert!(out.contains(&slug), "{out}");
}

#[test]
fn unpublish_without_a_published_page_says_so() {
    let server = FakeServer::start();
    let repo = repo_with_history(1);

    let err =
        madelog::run_capture(repo.root(), &["unpublish", "--server", &server.url]).unwrap_err();
    assert!(format!("{err:#}").contains("no published page"), "{err:#}");
    assert!(server.requests().is_empty());
}

#[test]
fn publish_outside_a_madelog_repository_refuses() {
    let repo = init_repo();
    isolate_device_key();
    let err = madelog::run_capture(repo.root(), &["publish", "--local", "--yes"]).unwrap_err();
    let text = format!("{err:#}");
    assert!(text.contains("no madelog log"), "{text}");
    assert!(text.contains("madelog init"), "{text}");
}

#[test]
fn a_server_that_refuses_the_signature_is_explained_in_plain_english() {
    let server = FakeServer::start();
    let repo = repo_with_history(3);

    // Register a device, then point the config at a device id that exists but
    // belongs to a key madelog does not have.
    madelog::run_capture(repo.root(), &["publish", "--yes", "--server", &server.url]).unwrap();
    {
        let mut state = server.state.lock().unwrap();
        let id = state.devices.keys().next().unwrap().clone();
        state.devices.insert(id, "ab".repeat(32));
    }

    let err = madelog::run_capture(repo.root(), &["publish", "--yes", "--server", &server.url])
        .unwrap_err();
    let text = format!("{err:#}");
    assert!(text.contains("signature"), "{text}");
    assert!(text.contains("device.key"), "{text}");
}

#[test]
fn nothing_listening_leaves_the_page_on_disk_and_says_why() {
    let repo = repo_with_history(3);
    let dead = {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", listener.local_addr().unwrap())
    };

    let err =
        madelog::run_capture(repo.root(), &["publish", "--yes", "--server", &dead]).unwrap_err();
    let text = format!("{err:#}");
    assert!(text.contains("could not reach"), "{text}");
    assert_eq!(
        text.matches("could not reach").count(),
        1,
        "the transport error is printed once, not once per link in the chain: {text}"
    );
    assert!(
        repo.path(".madelog/site/index.html").is_file(),
        "the page is written before the upload is attempted"
    );
}

#[test]
fn an_empty_server_flag_is_refused_rather_than_ignored() {
    let repo = repo_with_history(1);
    madelog::run_capture(
        repo.root(),
        &["config", "set", "server.url", "https://madelog.example"],
    )
    .unwrap();

    for args in [
        vec!["publish", "--yes", "--server", ""],
        vec!["publish", "--local", "--yes", "--server", "  "],
        vec!["unpublish", "--server", ""],
    ] {
        let before = std::fs::read_to_string(repo.path(".madelog/site/index.html")).unwrap();
        let err = madelog::run_capture(repo.root(), &args).unwrap_err();
        let text = format!("{err:#}");
        assert_eq!(exit_code(&err), Some(2), "{args:?}: {text}");
        assert!(text.contains("--server"), "{args:?}: {text}");
        assert_eq!(
            std::fs::read_to_string(repo.path(".madelog/site/index.html")).unwrap(),
            before,
            "{args:?} renders nothing of its own"
        );
    }
}
