//! `.madelog/config.toml` — the only knobs madelog has.

use std::path::Path;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

/// Every key `madelog config get|set` accepts.
pub const KEYS: &[&str] = &[
    "llm.provider",
    "llm.model",
    "llm.remote",
    "server.url",
    "server.device_id",
    "server.slug",
    "privacy.redact",
    "project.name",
    "project.delivered_url",
    "project.screenshot",
];

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub llm: Llm,
    pub server: Server,
    pub privacy: Privacy,
    pub project: Project,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Llm {
    /// `auto | ollama | none`
    pub provider: String,
    pub model: String,
    /// Remote APIs are never used unless you turn this on.
    pub remote: bool,
}

impl Default for Llm {
    fn default() -> Self {
        Llm {
            provider: "auto".into(),
            model: "qwen3:4b".into(),
            remote: false,
        }
    }
}

/// Where `madelog publish` sends the page. The default is the hosted server as
/// it is deployed today; `madelog.app` serves the site and the install scripts
/// and will take this over once it has a custom domain (`docs/deploy.md`).
pub const DEFAULT_SERVER_URL: &str = "https://madelog-server-production.up.railway.app";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Server {
    pub url: String,
    /// What `POST /v1/devices` called this machine. Written by `madelog publish`,
    /// not by you; it is a public identifier, never a secret.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    /// The page this repository already owns, so re-publishing replaces it
    /// instead of minting a second link.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
}

impl Default for Server {
    fn default() -> Self {
        Server {
            url: DEFAULT_SERVER_URL.into(),
            device_id: None,
            slug: None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Privacy {
    /// Regexes. Matching paths never contribute a diff excerpt, and matching
    /// prompts are dropped from AI session events.
    pub redact: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Project {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delivered_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot: Option<String>,
}

impl Config {
    /// Load `path`, or return defaults if it does not exist yet.
    pub fn load(path: &Path) -> Result<Config> {
        if !path.exists() {
            return Ok(Config::default());
        }
        let raw =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&raw).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let body = toml::to_string_pretty(self).context("serialising config")?;
        let header =
            "# madelog configuration. `madelog config set <key> <value>` edits this file.\n";
        std::fs::write(path, format!("{header}{body}"))
            .with_context(|| format!("writing {}", path.display()))
    }

    pub fn get(&self, key: &str) -> Result<String> {
        Ok(match key {
            "llm.provider" => self.llm.provider.clone(),
            "llm.model" => self.llm.model.clone(),
            "llm.remote" => self.llm.remote.to_string(),
            "server.url" => self.server.url.clone(),
            "server.device_id" => self.server.device_id.clone().unwrap_or_default(),
            "server.slug" => self.server.slug.clone().unwrap_or_default(),
            "privacy.redact" => self.privacy.redact.join("\n"),
            "project.name" => self.project.name.clone(),
            "project.delivered_url" => self.project.delivered_url.clone().unwrap_or_default(),
            "project.screenshot" => self.project.screenshot.clone().unwrap_or_default(),
            other => return Err(unknown_key(other)),
        })
    }

    pub fn set(&mut self, key: &str, value: &str) -> Result<()> {
        match key {
            "llm.provider" => {
                if !matches!(value, "auto" | "ollama" | "none") {
                    return Err(anyhow!(
                        "llm.provider must be one of auto, ollama, none (got `{value}`)"
                    ));
                }
                self.llm.provider = value.to_string();
            }
            "llm.model" => self.llm.model = value.to_string(),
            "llm.remote" => self.llm.remote = parse_bool(value)?,
            "server.url" => self.server.url = value.to_string(),
            "server.device_id" => self.server.device_id = optional(value),
            "server.slug" => self.server.slug = optional(value),
            "privacy.redact" => {
                let list: Vec<String> = value
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect();
                // Fail early rather than at the next commit.
                crate::gitrepo::ExcerptFilter::new(&list)?;
                self.privacy.redact = list;
            }
            "project.name" => self.project.name = value.to_string(),
            "project.delivered_url" => self.project.delivered_url = optional(value),
            "project.screenshot" => self.project.screenshot = optional(value),
            other => return Err(unknown_key(other)),
        }
        Ok(())
    }
}

fn optional(value: &str) -> Option<String> {
    if value.trim().is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn unknown_key(key: &str) -> anyhow::Error {
    anyhow!(
        "unknown config key `{key}`. Known keys: {}",
        KEYS.join(", ")
    )
}

fn parse_bool(value: &str) -> Result<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "y" | "1" | "on" => Ok(true),
        "false" | "no" | "n" | "0" | "off" => Ok(false),
        other => Err(anyhow!("expected true or false, got `{other}`")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_keep_everything_local() {
        let c = Config::default();
        assert_eq!(c.llm.provider, "auto");
        assert!(!c.llm.remote, "remote models are opt-in");
        assert!(c.privacy.redact.is_empty());
        assert_eq!(c.project.delivered_url, None);
    }

    #[test]
    fn a_missing_file_loads_as_defaults() {
        let dir = tempfile::TempDir::new().unwrap();
        assert_eq!(
            Config::load(&dir.path().join("config.toml")).unwrap(),
            Config::default()
        );
    }

    #[test]
    fn config_round_trips_through_toml() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        let mut c = Config::default();
        c.set("privacy.redact", "^secrets/, [.]env$").unwrap();
        c.set("project.name", "madelog").unwrap();
        c.set("project.delivered_url", "https://a.test").unwrap();
        c.set("llm.remote", "yes").unwrap();
        c.save(&path).unwrap();

        let back = Config::load(&path).unwrap();
        assert_eq!(back, c);
        assert_eq!(back.privacy.redact, vec!["^secrets/", "[.]env$"]);
        assert!(back.llm.remote);
    }

    #[test]
    fn every_documented_key_gets_and_sets() {
        let mut c = Config::default();
        for key in KEYS {
            c.get(key).unwrap();
        }
        c.set("llm.provider", "ollama").unwrap();
        assert_eq!(c.get("llm.provider").unwrap(), "ollama");
        assert!(c.set("llm.provider", "openai").is_err());
        assert!(c.set("llm.remote", "maybe").is_err());
        assert!(c.get("nope").is_err());
        assert!(c.set("nope", "x").is_err());
    }

    #[test]
    fn an_invalid_redact_regex_is_rejected_at_set_time() {
        let mut c = Config::default();
        assert!(c.set("privacy.redact", "(unclosed").is_err());
        assert!(c.privacy.redact.is_empty());
    }

    #[test]
    fn clearing_an_optional_value_stores_nothing() {
        let mut c = Config::default();
        c.set("project.screenshot", "shot.png").unwrap();
        c.set("project.screenshot", "").unwrap();
        assert_eq!(c.project.screenshot, None);
    }
}
