//! Talking to the madelog server: the exact bytes we sign, and the three requests
//! `madelog publish` and `madelog unpublish` make.
//!
//! The signature scheme is `server/README.md`, and it is deliberately dull:
//! the server verifies over the strings it received, so nothing here may
//! re-serialise, re-case or re-format a field between signing and sending.
//!
//! - publish: `root + "\n" + published_ts + "\n" + slug`, the slug empty on a
//!   first publish (so the message ends with a newline).
//! - delete:  `"delete" + "\n" + slug`.
//!
//! Everything on the wire is lowercase hex.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// The server refuses html larger than this, so we refuse it first and say why.
pub const MAX_HTML_BYTES: usize = 2 * 1024 * 1024;

/// Long enough for a slow link, short enough that a hung server is not a hang.
pub const TIMEOUT: Duration = Duration::from_secs(30);

/// The bytes `POST /v1/pages` verifies. `slug` is `""` on a first publish.
pub fn publish_message(root: &str, published_ts: &str, slug: &str) -> String {
    format!("{root}\n{published_ts}\n{slug}")
}

/// The bytes `DELETE /v1/pages/{slug}` verifies. The slug is never empty here.
pub fn delete_message(slug: &str) -> String {
    format!("delete\n{slug}")
}

/// `madelog.app`, `https://madelog.app/` and `https://madelog.app` all mean the same thing.
pub fn normalise_base(raw: &str) -> String {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    }
}

// ---------------------------------------------------------------------------
// errors
// ---------------------------------------------------------------------------

/// What went wrong, in words the person at the keyboard can act on.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// 404 on `/v1/pages`: the server has never heard of this `device_id`.
    /// The caller registers again and retries once.
    #[error("the server does not know this device any more")]
    UnknownDevice,
    /// The server answered, and said no.
    #[error("{message}")]
    Refused { status: u16, message: String },
    /// Nothing answered, or what answered was not the madelog server.
    ///
    /// The inner error is *not* declared as a source: its Display is already
    /// the whole of this error's Display, and anyhow's `{:#}` would walk the
    /// chain and print the same sentence a second time.
    #[error("{0:#}")]
    Transport(anyhow::Error),
}

impl ApiError {
    pub fn status(&self) -> Option<u16> {
        match self {
            ApiError::UnknownDevice => Some(404),
            ApiError::Refused { status, .. } => Some(*status),
            ApiError::Transport(_) => None,
        }
    }
}

/// `{"error": "...", "message": "..."}` — the server's own words, when it has any.
#[derive(Debug, Default, Deserialize)]
struct ErrorBody {
    #[serde(default)]
    error: String,
    #[serde(default)]
    message: String,
}

fn server_words(body: &str) -> Option<String> {
    let parsed: ErrorBody = serde_json::from_str(body.trim()).ok()?;
    let words = if !parsed.message.trim().is_empty() {
        parsed.message
    } else {
        parsed.error
    };
    let words = words.trim();
    if words.is_empty() {
        None
    } else {
        Some(words.to_string())
    }
}

/// Plain English for every status the publish path can produce.
fn refusal(what: &str, status: u16, body: &str) -> ApiError {
    let said = server_words(body);
    let message = match status {
        400 => "the server could not read that request. This is a bug in madelog; \
                `madelog publish --local` still works."
            .to_string(),
        401 => "the server did not accept this device's signature. Your device key at \
                ~/.config/madelog/device.key does not match the key it has on file — \
                register again from this machine, or publish from the machine that \
                first published this page."
            .to_string(),
        403 => "that page belongs to another device. Publish from the machine that \
                created it, or run `madelog unpublish` there first."
            .to_string(),
        404 => format!("the server has nothing at that address ({what})."),
        413 => format!(
            "the page is larger than the {} MB the server accepts. Trim the project \
             page, or publish fewer sessions.",
            MAX_HTML_BYTES / (1024 * 1024)
        ),
        s if (500..=599).contains(&s) => {
            "the server is having trouble. Try again in a minute; your page is \
             already written locally."
                .to_string()
        }
        s => format!("the server answered {s} for {what}."),
    };
    let message = match said {
        Some(said) => format!("{message} The server said: {said}"),
        None => message,
    };
    ApiError::Refused { status, message }
}

// ---------------------------------------------------------------------------
// wire shapes
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct DeviceRequest<'a> {
    public_key: &'a str,
}

#[derive(Debug, Deserialize)]
struct DeviceResponse {
    #[serde(default)]
    device_id: String,
}

/// The body of `POST /v1/pages`. The server rejects unknown fields, so these
/// names are exactly the ones in `server/README.md` and nothing else is sent.
#[derive(Debug, Clone, Serialize)]
pub struct PageRequest {
    pub device_id: String,
    /// Omitted entirely on a first publish — never sent as an empty string.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    pub html: String,
    pub root: String,
    pub first_ts: String,
    pub last_ts: String,
    pub published_ts: String,
    pub signature: String,
}

/// `{slug, url, verify_url, received_at}`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PageResponse {
    pub slug: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub verify_url: String,
    #[serde(default)]
    pub received_at: String,
}

#[derive(Debug, Serialize)]
struct DeleteRequest<'a> {
    device_id: &'a str,
    signature: &'a str,
}

// ---------------------------------------------------------------------------
// client
// ---------------------------------------------------------------------------

/// A blocking client for the three endpoints madelog uses.
pub struct Client {
    base: String,
    agent: ureq::Agent,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client").field("base", &self.base).finish()
    }
}

impl Client {
    pub fn new(base_url: &str) -> Client {
        Client {
            base: normalise_base(base_url),
            // Statuses are not errors here: a 403 carries a body worth reading.
            agent: ureq::Agent::config_builder()
                .timeout_global(Some(TIMEOUT))
                .http_status_as_error(false)
                .build()
                .new_agent(),
        }
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    /// `POST /v1/devices` — idempotent on the public key, so calling it on
    /// every publish is free and always returns the same id.
    pub fn register_device(&self, public_key_hex: &str) -> Result<String, ApiError> {
        let url = format!("{}/v1/devices", self.base);
        let (status, body) = self.send_json(
            "POST",
            &url,
            &serde_json::to_string(&DeviceRequest {
                public_key: public_key_hex,
            })
            .expect("a device request always serialises"),
        )?;
        if status != 200 && status != 201 {
            return Err(refusal("registering this device", status, &body));
        }
        let parsed: DeviceResponse = serde_json::from_str(body.trim()).map_err(|e| {
            ApiError::Transport(anyhow::anyhow!(
                "{url} did not answer with a device id ({e}); is that really a madelog server?"
            ))
        })?;
        if parsed.device_id.trim().is_empty() {
            return Err(ApiError::Transport(anyhow::anyhow!(
                "{url} answered with an empty device id"
            )));
        }
        Ok(parsed.device_id)
    }

    /// `POST /v1/pages`.
    pub fn publish_page(&self, request: &PageRequest) -> Result<PageResponse, ApiError> {
        let url = format!("{}/v1/pages", self.base);
        let body = serde_json::to_string(request)
            .map_err(|e| ApiError::Transport(anyhow::anyhow!("building the request: {e}")))?;
        let (status, answer) = self.send_json("POST", &url, &body)?;
        if status == 404 {
            return Err(ApiError::UnknownDevice);
        }
        if status != 200 && status != 201 {
            return Err(refusal("publishing the page", status, &answer));
        }
        let parsed: PageResponse = serde_json::from_str(answer.trim()).map_err(|e| {
            ApiError::Transport(anyhow::anyhow!(
                "{url} did not answer with a published page ({e}); is that really a madelog server?"
            ))
        })?;
        Ok(parsed)
    }

    /// `DELETE /v1/pages/{slug}` with `{device_id, signature}`.
    pub fn delete_page(
        &self,
        slug: &str,
        device_id: &str,
        signature_hex: &str,
    ) -> Result<(), ApiError> {
        let url = format!("{}/v1/pages/{slug}", self.base);
        let body = serde_json::to_string(&DeleteRequest {
            device_id,
            signature: signature_hex,
        })
        .expect("a delete request always serialises");
        let (status, answer) = self.send_json("DELETE", &url, &body)?;
        // Deleting twice is fine, and so is deleting a page the server has
        // already forgotten: the point was that it is gone.
        if status == 200 || status == 204 || status == 404 {
            return Ok(());
        }
        Err(refusal("deleting the page", status, &answer))
    }

    fn send_json(&self, method: &str, url: &str, body: &str) -> Result<(u16, String), ApiError> {
        let request = ureq::http::Request::builder()
            .method(method)
            .uri(url)
            .header("content-type", "application/json")
            .header("accept", "application/json")
            .body(body.to_string())
            .map_err(|e| {
                ApiError::Transport(anyhow::anyhow!("building a request to {url}: {e}"))
            })?;

        let mut response = self.agent.run(request).map_err(|e| {
            ApiError::Transport(anyhow::anyhow!(
                "could not reach {url}: {e}. Your page is written locally either way."
            ))
        })?;
        let status = response.status().as_u16();
        let text = response.body_mut().read_to_string().map_err(|e| {
            ApiError::Transport(anyhow::anyhow!("reading the answer from {url}: {e}"))
        })?;
        Ok((status, text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;

    /// A throwaway HTTP server: one canned answer, and it remembers what it saw.
    fn serve_once(status: u16, body: &'static str) -> (String, std::sync::mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = format!("http://{}", listener.local_addr().unwrap());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for stream in listener.incoming().take(4) {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let mut seen = line.trim_end().to_string();
                let mut length = 0usize;
                loop {
                    let mut header = String::new();
                    if reader.read_line(&mut header).unwrap() == 0 || header.trim().is_empty() {
                        break;
                    }
                    if let Some(v) = header.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = v.trim().parse().unwrap_or(0);
                    }
                }
                let mut raw = vec![0u8; length];
                if length > 0 {
                    reader.read_exact(&mut raw).unwrap();
                }
                seen.push('\n');
                seen.push_str(&String::from_utf8_lossy(&raw));
                let _ = tx.send(seen);
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.flush();
            }
        });
        (addr, rx)
    }

    #[test]
    fn the_publish_message_is_the_documented_byte_string() {
        let root = "3d9f1c0a".to_string() + &"0".repeat(56);
        // First publish: no slug, so the message ends with a newline.
        assert_eq!(
            publish_message(&root, "2026-09-17T14:05:00Z", ""),
            format!("{root}\n2026-09-17T14:05:00Z\n")
        );
        assert!(publish_message(&root, "2026-09-17T14:05:00Z", "").ends_with('\n'));
        // Re-publishing: the slug is the last line, with no trailing newline.
        assert_eq!(
            publish_message(&root, "2026-09-17T14:05:00Z", "quiet-heron-k7m2"),
            format!("{root}\n2026-09-17T14:05:00Z\nquiet-heron-k7m2")
        );
        // Exactly two newlines either way: three fields, no separator drift.
        assert_eq!(
            publish_message(&root, "2026-09-17T14:05:00Z", "quiet-heron-k7m2")
                .matches('\n')
                .count(),
            2
        );
    }

    #[test]
    fn the_delete_message_is_the_documented_byte_string() {
        assert_eq!(
            delete_message("quiet-heron-k7m2"),
            "delete\nquiet-heron-k7m2"
        );
        assert_eq!(delete_message("x").as_bytes(), b"delete\nx");
    }

    #[test]
    fn a_bare_host_gets_https_and_loses_its_slash() {
        assert_eq!(normalise_base("madelog.app"), "https://madelog.app");
        assert_eq!(
            normalise_base("https://madelog.app/"),
            "https://madelog.app"
        );
        assert_eq!(
            normalise_base(" http://127.0.0.1:8080/ "),
            "http://127.0.0.1:8080"
        );
    }

    #[test]
    fn registering_sends_only_the_public_key() {
        let (addr, seen) = serve_once(200, r#"{"device_id":"dev-1"}"#);
        let id = Client::new(&addr)
            .register_device(&"ab".repeat(32))
            .unwrap();
        assert_eq!(id, "dev-1");
        let request = seen.recv().unwrap();
        assert!(request.starts_with("POST /v1/devices "), "{request}");
        let body: serde_json::Value =
            serde_json::from_str(request.split_once('\n').unwrap().1).unwrap();
        assert_eq!(body, serde_json::json!({ "public_key": "ab".repeat(32) }));
    }

    #[test]
    fn a_first_publish_omits_the_slug_field_entirely() {
        let (addr, seen) = serve_once(
            200,
            r#"{"slug":"quiet-heron-k7m2","url":"https://madelog.app/p/quiet-heron-k7m2","verify_url":"https://madelog.app/v1/verify/quiet-heron-k7m2","received_at":"2026-09-17T14:05:01Z"}"#,
        );
        let response = Client::new(&addr)
            .publish_page(&PageRequest {
                device_id: "dev-1".into(),
                slug: None,
                html: "<h1>hi</h1>".into(),
                root: "ab".repeat(32),
                first_ts: "2026-09-01T09:00:00Z".into(),
                last_ts: "2026-09-17T14:00:00Z".into(),
                published_ts: "2026-09-17T14:05:00Z".into(),
                signature: "cd".repeat(64),
            })
            .unwrap();
        assert_eq!(response.slug, "quiet-heron-k7m2");
        assert_eq!(
            response.verify_url,
            "https://madelog.app/v1/verify/quiet-heron-k7m2"
        );

        let request = seen.recv().unwrap();
        let body: serde_json::Value =
            serde_json::from_str(request.split_once('\n').unwrap().1).unwrap();
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
            ],
            "no slug key, and no field the server would reject"
        );
    }

    #[test]
    fn an_unknown_device_is_its_own_error_so_the_caller_can_retry() {
        let (addr, _seen) = serve_once(
            404,
            r#"{"error":"unknown_device","message":"no such device"}"#,
        );
        let err = Client::new(&addr)
            .publish_page(&PageRequest {
                device_id: "gone".into(),
                slug: None,
                html: "<h1>hi</h1>".into(),
                root: "ab".repeat(32),
                first_ts: "2026-09-01T09:00:00Z".into(),
                last_ts: "2026-09-17T14:00:00Z".into(),
                published_ts: "2026-09-17T14:05:00Z".into(),
                signature: "cd".repeat(64),
            })
            .unwrap_err();
        assert!(matches!(err, ApiError::UnknownDevice), "{err}");
    }

    #[test]
    fn refusals_are_translated_into_plain_english() {
        for (status, needle) in [
            (401u16, "signature"),
            (403, "belongs to another device"),
            (413, "larger than the 2 MB"),
        ] {
            let (addr, _seen) = serve_once(status, r#"{"error":"nope","message":"server words"}"#);
            let err = Client::new(&addr)
                .publish_page(&PageRequest {
                    device_id: "dev-1".into(),
                    slug: Some("quiet-heron-k7m2".into()),
                    html: "<h1>hi</h1>".into(),
                    root: "ab".repeat(32),
                    first_ts: "2026-09-01T09:00:00Z".into(),
                    last_ts: "2026-09-17T14:00:00Z".into(),
                    published_ts: "2026-09-17T14:05:00Z".into(),
                    signature: "cd".repeat(64),
                })
                .unwrap_err();
            let text = err.to_string();
            assert!(text.contains(needle), "{status}: {text}");
            assert!(text.contains("server words"), "{status}: {text}");
            assert_eq!(err.status(), Some(status));
        }
    }

    #[test]
    fn delete_sends_a_body_and_treats_a_missing_page_as_done() {
        let (addr, seen) = serve_once(200, r#"{"slug":"quiet-heron-k7m2","deleted":true}"#);
        Client::new(&addr)
            .delete_page("quiet-heron-k7m2", "dev-1", &"ef".repeat(64))
            .unwrap();
        let request = seen.recv().unwrap();
        assert!(
            request.starts_with("DELETE /v1/pages/quiet-heron-k7m2 "),
            "{request}"
        );
        let body: serde_json::Value =
            serde_json::from_str(request.split_once('\n').unwrap().1).unwrap();
        assert_eq!(
            body,
            serde_json::json!({ "device_id": "dev-1", "signature": "ef".repeat(64) })
        );

        let (gone, _) = serve_once(404, r#"{"error":"not_found"}"#);
        Client::new(&gone)
            .delete_page("quiet-heron-k7m2", "dev-1", &"ef".repeat(64))
            .unwrap();
    }

    #[test]
    fn a_transport_error_says_its_piece_once() {
        let err: anyhow::Error =
            ApiError::Transport(anyhow::anyhow!("could not reach http://127.0.0.1:9")).into();
        let text = format!("{err:#}");
        assert_eq!(
            text.matches("could not reach http://127.0.0.1:9").count(),
            1,
            "the anyhow chain must not repeat the message: {text}"
        );
    }

    #[test]
    fn nothing_listening_is_a_transport_error_that_names_the_url() {
        let addr = {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            format!("http://{}", l.local_addr().unwrap())
        };
        let err = Client::new(&addr)
            .register_device(&"ab".repeat(32))
            .unwrap_err();
        assert!(matches!(err, ApiError::Transport(_)), "{err}");
        assert!(err.to_string().contains("/v1/devices"), "{err}");
    }
}
