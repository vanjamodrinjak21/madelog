//! A small client for a local Ollama, and nothing else.
//!
//! madelog talks to `http://localhost:11434` if something is listening there. It
//! asks for JSON, validates what comes back, and treats anything unexpected as
//! "no model available" rather than as text worth publishing.

use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;

/// Where Ollama listens unless you say otherwise.
pub const DEFAULT_HOST: &str = "http://localhost:11434";

/// The model madelog asks for when the config does not name one.
pub const DEFAULT_MODEL: &str = "qwen3:4b";

/// Is it alive? One second is long enough to answer on localhost.
const PROBE_TIMEOUT: Duration = Duration::from_secs(1);

/// Generating a few paragraphs on a laptop CPU is slow. Not unbounded, though.
const GENERATE_TIMEOUT: Duration = Duration::from_secs(60);

/// A summary longer than this is a model that has stopped summarising.
pub const MAX_SUMMARY_CHARS: usize = 2000;

/// What the model is told it is doing. Facts in, JSON out, nothing invented.
pub const SYSTEM_PROMPT: &str = "You summarise one week of a single developer's work for a page a \
company will read. You are given commit messages, decisions the developer wrote down, and \
patterns a program detected. Use only those facts. Never invent a reason, a feeling or a result. \
Do not judge the work. Answer with JSON only, of the form {\"summary\": \"...\"}, where the \
summary is two or three short paragraphs of plain English separated by blank lines: what \
happened, what was decided, what stood out.";

/// Which host to talk to: the config key if the config has one, then
/// `OLLAMA_HOST`, then localhost.
pub fn host(config: &crate::config::Config) -> String {
    // `llm.host` is not a key madelog v1 writes; reading it through `get` means a
    // later version can add it without this file changing.
    let configured = config.get("llm.host").ok().filter(|v| !v.trim().is_empty());
    let raw = configured
        .or_else(|| std::env::var("OLLAMA_HOST").ok())
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_HOST.to_string());
    normalise_host(&raw)
}

/// `localhost:11434` and `http://localhost:11434/` mean the same thing.
pub fn normalise_host(raw: &str) -> String {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("http://{trimmed}")
    }
}

#[derive(Deserialize)]
struct TagsResponse {
    #[serde(default)]
    models: Vec<TagEntry>,
}

#[derive(Deserialize)]
struct TagEntry {
    #[serde(default)]
    name: String,
    #[serde(default)]
    model: String,
}

#[derive(Deserialize)]
struct GenerateResponse {
    #[serde(default)]
    response: String,
}

#[derive(Deserialize)]
struct SummaryEnvelope {
    #[serde(default)]
    summary: String,
}

pub struct Ollama {
    host: String,
    probe: ureq::Agent,
    work: ureq::Agent,
}

impl std::fmt::Debug for Ollama {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ollama").field("host", &self.host).finish()
    }
}

impl Ollama {
    pub fn new(host: &str) -> Ollama {
        Ollama {
            host: normalise_host(host),
            probe: agent(PROBE_TIMEOUT),
            work: agent(GENERATE_TIMEOUT),
        }
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    /// The models Ollama has pulled, as it names them.
    pub fn models(&self) -> Result<Vec<String>> {
        let url = format!("{}/api/tags", self.host);
        let tags: TagsResponse = self
            .probe
            .get(&url)
            .call()
            .with_context(|| format!("asking {url} for its models"))?
            .body_mut()
            .read_json()
            .with_context(|| format!("reading the model list from {url}"))?;
        Ok(tags
            .models
            .into_iter()
            .map(|m| if m.name.is_empty() { m.model } else { m.name })
            .filter(|n| !n.is_empty())
            .collect())
    }

    /// Is something answering on the Ollama API at all?
    pub fn is_available(&self) -> bool {
        self.models().is_ok()
    }

    /// Has this model been pulled? `qwen3:4b` also matches `qwen3:4b` reported
    /// with an explicit `:latest`, and a bare name matches its `:latest` tag.
    pub fn has_model(&self, name: &str) -> bool {
        let Ok(models) = self.models() else {
            return false;
        };
        models.iter().any(|have| same_model(have, name))
    }

    /// Ask for a summary and return it only if it is usable.
    ///
    /// Anything else — a refused connection, a non-JSON body, an empty or
    /// runaway summary — is an error, and the caller falls back to the
    /// template rather than publishing whatever came back.
    pub fn generate(&self, model: &str, system: &str, prompt: &str) -> Result<String> {
        let url = format!("{}/api/generate", self.host);
        let body = serde_json::json!({
            "model": model,
            "system": system,
            "prompt": prompt,
            "format": "json",
            "stream": false,
        });
        let generated: GenerateResponse = self
            .work
            .post(&url)
            .send_json(&body)
            .with_context(|| format!("asking {model} at {} for a summary", self.host))?
            .body_mut()
            .read_json()
            .with_context(|| format!("reading the answer from {url}"))?;
        parse_summary(&generated.response)
    }
}

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .build()
        .new_agent()
}

fn same_model(have: &str, want: &str) -> bool {
    if have == want {
        return true;
    }
    let with_latest = |n: &str| {
        if n.contains(':') {
            n.to_string()
        } else {
            format!("{n}:latest")
        }
    };
    with_latest(have) == with_latest(want)
}

/// `{"summary": "..."}` — validated, because a model is a guest here.
fn parse_summary(raw: &str) -> Result<String> {
    let envelope: SummaryEnvelope = serde_json::from_str(raw.trim())
        .map_err(|e| anyhow!("the model did not answer with {{\"summary\": ...}} JSON: {e}"))?;
    let summary = envelope.summary.trim();
    if summary.is_empty() {
        bail!("the model answered with an empty summary");
    }
    if summary.chars().count() > MAX_SUMMARY_CHARS {
        bail!(
            "the model answered with {} characters, more than the {MAX_SUMMARY_CHARS} a summary may have",
            summary.chars().count()
        );
    }
    Ok(summary.to_string())
}

/// A throwaway Ollama for the tests in this crate.
#[cfg(test)]
pub(crate) mod fake {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};

    /// A throwaway HTTP server. Enough of the protocol to answer ureq, and
    /// nothing more — the point is to test our side, not to write a server.
    pub(crate) struct FakeOllama {
        addr: String,
        seen: Arc<Mutex<Vec<String>>>,
    }

    impl FakeOllama {
        /// `reply` maps a request path and body to (status, response body).
        pub(crate) fn start(
            reply: impl Fn(&str, &str) -> (u16, String) + Send + Sync + 'static,
        ) -> FakeOllama {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind a port");
            let addr = format!("http://{}", listener.local_addr().unwrap());
            let seen = Arc::new(Mutex::new(Vec::new()));
            let recorded = Arc::clone(&seen);
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else { continue };
                    if let Err(err) = serve(stream, &reply, &recorded) {
                        eprintln!("fake ollama: {err}");
                    }
                }
            });
            FakeOllama { addr, seen }
        }

        pub(crate) fn addr(&self) -> &str {
            &self.addr
        }

        pub(crate) fn client(&self) -> super::Ollama {
            super::Ollama::new(&self.addr)
        }

        pub(crate) fn bodies(&self) -> Vec<String> {
            self.seen.lock().unwrap().clone()
        }
    }

    fn serve(
        mut stream: TcpStream,
        reply: &(impl Fn(&str, &str) -> (u16, String) + ?Sized),
        seen: &Mutex<Vec<String>>,
    ) -> std::io::Result<()> {
        let mut reader = BufReader::new(stream.try_clone()?);
        let mut request_line = String::new();
        reader.read_line(&mut request_line)?;
        let path = request_line
            .split_whitespace()
            .nth(1)
            .unwrap_or("/")
            .to_string();

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
        let mut body = vec![0u8; length];
        if length > 0 {
            reader.read_exact(&mut body)?;
        }
        let body = String::from_utf8_lossy(&body).into_owned();
        seen.lock().unwrap().push(body.clone());

        let (status, payload) = reply(&path, &body);
        write!(
            stream,
            "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
            payload.len()
        )?;
        stream.flush()
    }

    /// The tags of a machine that has `qwen3:4b` and `llama3` pulled, plus a
    /// fixed answer from `/api/generate`.
    pub(crate) fn tags_and_generate(
        response: &'static str,
    ) -> impl Fn(&str, &str) -> (u16, String) {
        move |path, _| match path {
            "/api/tags" => (
                200,
                r#"{"models":[{"name":"qwen3:4b","model":"qwen3:4b"},{"name":"llama3:latest"}]}"#
                    .to_string(),
            ),
            "/api/generate" => (200, response.to_string()),
            _ => (404, "{}".to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::{tags_and_generate, FakeOllama};
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn a_host_without_a_scheme_gets_one() {
        assert_eq!(normalise_host("localhost:11434"), "http://localhost:11434");
        assert_eq!(
            normalise_host("http://localhost:11434/"),
            "http://localhost:11434"
        );
        assert_eq!(normalise_host(" https://box.lan "), "https://box.lan");
    }

    #[test]
    fn a_running_ollama_reports_its_models() {
        let server = FakeOllama::start(tags_and_generate("{}"));
        let client = server.client();
        assert!(client.is_available());
        assert!(client.has_model("qwen3:4b"));
        assert!(client.has_model("llama3"), "a bare name matches :latest");
        assert!(!client.has_model("mistral"));
    }

    #[test]
    fn nothing_listening_is_simply_unavailable() {
        // Bind a port, learn its number, drop it: nothing is listening there.
        let addr = {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap()
        };
        let client = Ollama::new(&format!("http://{addr}"));
        assert!(!client.is_available());
        assert!(!client.has_model("qwen3:4b"));
        assert!(client.generate("qwen3:4b", "s", "p").is_err());
    }

    #[test]
    fn an_ollama_that_answers_rubbish_is_unavailable() {
        let server = FakeOllama::start(|path, _| match path {
            "/api/tags" => (200, "<html>not ollama</html>".to_string()),
            _ => (404, "{}".to_string()),
        });
        assert!(!server.client().is_available());
    }

    #[test]
    fn a_good_answer_comes_back_as_text() {
        let server = FakeOllama::start(tags_and_generate(
            r#"{"response":"{\"summary\":\"You moved the store.\\n\\nYou wrote it down.\"}"}"#,
        ));
        let text = server
            .client()
            .generate("qwen3:4b", SYSTEM_PROMPT, "Week 1: facts")
            .unwrap();
        assert_eq!(text, "You moved the store.\n\nYou wrote it down.");

        let sent: serde_json::Value =
            serde_json::from_str(server.bodies().last().unwrap()).unwrap();
        assert_eq!(sent["format"], "json");
        assert_eq!(sent["stream"], false);
        assert_eq!(sent["model"], "qwen3:4b");
        assert_eq!(sent["prompt"], "Week 1: facts");
    }

    #[test]
    fn an_answer_that_is_not_json_is_refused() {
        let server = FakeOllama::start(tags_and_generate(
            r#"{"response":"Sure! Here is your summary."}"#,
        ));
        let err = server
            .client()
            .generate("qwen3:4b", SYSTEM_PROMPT, "facts")
            .unwrap_err();
        assert!(format!("{err:#}").contains("summary"), "{err:#}");
    }

    #[test]
    fn an_empty_summary_is_refused() {
        let server = FakeOllama::start(tags_and_generate(r#"{"response":"{\"summary\":\"  \"}"}"#));
        let err = server
            .client()
            .generate("qwen3:4b", SYSTEM_PROMPT, "facts")
            .unwrap_err();
        assert!(format!("{err:#}").contains("empty"), "{err:#}");
    }

    #[test]
    fn a_runaway_summary_is_refused() {
        let long = "word ".repeat(MAX_SUMMARY_CHARS);
        let body = serde_json::json!({
            "response": serde_json::json!({ "summary": long }).to_string()
        })
        .to_string();
        let server = FakeOllama::start(move |path, _| match path {
            "/api/generate" => (200, body.clone()),
            _ => (200, r#"{"models":[]}"#.to_string()),
        });
        let err = server
            .client()
            .generate("qwen3:4b", SYSTEM_PROMPT, "facts")
            .unwrap_err();
        assert!(format!("{err:#}").contains("2000"), "{err:#}");
    }

    #[test]
    fn a_summary_at_the_limit_is_kept() {
        assert_eq!(
            parse_summary(
                &serde_json::json!({"summary": "x".repeat(MAX_SUMMARY_CHARS)}).to_string()
            )
            .unwrap()
            .len(),
            MAX_SUMMARY_CHARS
        );
    }

    #[test]
    fn the_host_falls_back_from_config_to_env_to_localhost() {
        let config = crate::config::Config::default();
        // `llm.host` is not a key yet, so the environment decides.
        std::env::remove_var("OLLAMA_HOST");
        assert_eq!(host(&config), DEFAULT_HOST);
        std::env::set_var("OLLAMA_HOST", "127.0.0.1:1234");
        assert_eq!(host(&config), "http://127.0.0.1:1234");
        std::env::remove_var("OLLAMA_HOST");
    }
}
