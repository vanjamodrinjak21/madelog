//! Weekly summaries (spec §4.3).
//!
//! The week is bucketed in [`weeks`], turned into structured text there, and
//! handed either to a local model ([`ollama`]) or to the template
//! ([`heuristic`]). Nothing here ever sees a diff or a prompt, and nothing
//! here ever calls a remote API: `llm.remote` is out of scope in this version.

pub mod heuristic;
pub mod ollama;
pub mod weeks;

use std::collections::BTreeMap;

use anyhow::{bail, Result};

use crate::config::Config;
use crate::event::{Event, EventKind, SummaryPayload};

pub use ollama::{Ollama, DEFAULT_MODEL};
pub use weeks::{bucket, Week};

/// Who writes the summaries this run.
#[derive(Debug)]
pub enum Provider {
    Ollama { model: String, client: Box<Ollama> },
    Heuristic,
}

/// One written summary, and whatever the user should know about how it came
/// about.
#[derive(Debug)]
pub struct Written {
    pub text: String,
    /// `ollama:<model>` or `heuristic`, exactly as the payload records it.
    pub generated_by: String,
    /// Printed once when a model was asked and could not deliver.
    pub note: Option<String>,
}

impl Provider {
    /// Write one week. A model that fails is not an error: the template is
    /// always there, and a page that says less is better than a page that says
    /// something made up.
    pub fn write(&self, week: &Week) -> Written {
        match self {
            Provider::Heuristic => Written {
                text: heuristic::summarize(week),
                generated_by: "heuristic".to_string(),
                note: None,
            },
            Provider::Ollama { model, client } => {
                match client.generate(model, ollama::SYSTEM_PROMPT, &week.input_text()) {
                    Ok(text) => Written {
                        text,
                        generated_by: format!("ollama:{model}"),
                        note: None,
                    },
                    Err(err) => Written {
                        text: heuristic::summarize(week),
                        generated_by: "heuristic".to_string(),
                        note: Some(format!(
                            "week {}: {model} did not return a usable summary ({err:#}); \
                             wrote the template instead",
                            week.index
                        )),
                    },
                }
            }
        }
    }
}

/// A provider plus the lines the command should print before using it.
#[derive(Debug)]
pub struct Choice {
    pub provider: Provider,
    pub notices: Vec<String>,
}

/// Resolve `llm.provider` against what is actually running on this machine.
///
/// - `auto`: Ollama if it answers and has the model, otherwise the template.
/// - `ollama`: Ollama or an error — you asked for it explicitly.
/// - `none`: the template.
/// - `llm.remote = true`: refused, and the template.
pub fn choose_provider(config: &Config) -> Result<Choice> {
    choose_provider_at(config, &ollama::host(config))
}

fn choose_provider_at(config: &Config, host: &str) -> Result<Choice> {
    let mut notices = Vec::new();
    let model = model_name(config);

    if config.llm.remote {
        notices.push("remote models are not supported in this version".to_string());
        return Ok(Choice {
            provider: Provider::Heuristic,
            notices,
        });
    }

    match config.llm.provider.trim() {
        "none" => Ok(Choice {
            provider: Provider::Heuristic,
            notices,
        }),
        "ollama" => {
            let client = Ollama::new(host);
            if !client.is_available() {
                bail!(
                    "llm.provider is `ollama`, but nothing answers at {host}. \
                     Start Ollama, or run `madelog config set llm.provider auto`."
                );
            }
            if !client.has_model(&model) {
                bail!(
                    "llm.provider is `ollama`, but {host} has no model `{model}`. \
                     Run `ollama pull {model}`, or run `madelog config set llm.provider auto`."
                );
            }
            Ok(Choice {
                provider: Provider::Ollama {
                    model,
                    client: Box::new(client),
                },
                notices,
            })
        }
        // `auto`, and anything a future version writes that this one does not
        // understand: never fail, just write less.
        _ => {
            let client = Ollama::new(host);
            if !client.is_available() {
                notices.push(format!(
                    "no Ollama at {host}; writing the summaries from the template"
                ));
                return Ok(Choice {
                    provider: Provider::Heuristic,
                    notices,
                });
            }
            if !client.has_model(&model) {
                notices.push(format!(
                    "Ollama at {host} has no model `{model}`; writing the summaries from the \
                     template (run `ollama pull {model}` to use it)"
                ));
                return Ok(Choice {
                    provider: Provider::Heuristic,
                    notices,
                });
            }
            Ok(Choice {
                provider: Provider::Ollama {
                    model,
                    client: Box::new(client),
                },
                notices,
            })
        }
    }
}

fn model_name(config: &Config) -> String {
    let configured = config.llm.model.trim();
    if configured.is_empty() {
        DEFAULT_MODEL.to_string()
    } else {
        configured.to_string()
    }
}

/// The newest summary for each week, oldest week first.
///
/// Summaries are appended, never rewritten, so a week can carry several: the
/// generated one and every edit of it. The page shows the last.
pub fn latest_summaries(events: &[Event]) -> Vec<SummaryPayload> {
    let mut by_week: BTreeMap<String, SummaryPayload> = BTreeMap::new();
    for event in events.iter().filter(|e| e.kind == EventKind::Summary) {
        if let Ok(payload) = event.payload_as::<SummaryPayload>() {
            by_week.insert(payload.week_start.clone(), payload);
        }
    }
    by_week.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Payload;

    fn summary_event(seq: i64, week_start: &str, text: &str, generated_by: &str) -> Event {
        let payload = Payload::Summary(SummaryPayload {
            week_start: week_start.into(),
            week_end: "2026-09-20".into(),
            text: text.into(),
            generated_by: generated_by.into(),
            edited: generated_by == "user",
        });
        Event {
            seq,
            id: format!("id-{seq}"),
            ts: "2026-09-21T09:00:00Z".into(),
            kind: EventKind::Summary,
            commit_sha: None,
            payload: payload.to_value(),
            prev_hash: crate::event::GENESIS_PREV_HASH.into(),
            hash: "h".into(),
        }
    }

    fn config(provider: &str, remote: bool) -> Config {
        let mut config = Config::default();
        config.llm.provider = provider.to_string();
        config.llm.remote = remote;
        config
    }

    #[test]
    fn the_newest_summary_per_week_wins() {
        let events = vec![
            summary_event(1, "2026-09-07", "week one", "heuristic"),
            summary_event(2, "2026-09-14", "week two", "heuristic"),
            summary_event(3, "2026-09-14", "week two, in my words", "user"),
        ];
        let latest = latest_summaries(&events);
        assert_eq!(latest.len(), 2);
        assert_eq!(latest[0].week_start, "2026-09-07");
        assert_eq!(latest[1].text, "week two, in my words");
        assert!(latest[1].edited);
    }

    #[test]
    fn a_log_without_summaries_has_none() {
        assert!(latest_summaries(&[]).is_empty());
    }

    #[test]
    fn provider_none_never_touches_the_network() {
        // An address nothing could answer on: if this tried, it would fail.
        let choice = choose_provider_at(&config("none", false), "http://127.0.0.1:1").unwrap();
        assert!(matches!(choice.provider, Provider::Heuristic));
        assert!(choice.notices.is_empty());
    }

    #[test]
    fn remote_is_refused_and_the_template_takes_over() {
        let choice = choose_provider_at(&config("auto", true), "http://127.0.0.1:1").unwrap();
        assert!(matches!(choice.provider, Provider::Heuristic));
        assert_eq!(
            choice.notices,
            vec!["remote models are not supported in this version".to_string()]
        );
    }

    #[test]
    fn auto_falls_back_when_no_ollama_answers() {
        let choice = choose_provider_at(&config("auto", false), "http://127.0.0.1:1").unwrap();
        assert!(matches!(choice.provider, Provider::Heuristic));
        assert!(
            choice.notices[0].contains("no Ollama"),
            "{:?}",
            choice.notices
        );
    }

    #[test]
    fn asking_for_ollama_when_it_is_absent_is_an_error() {
        let err = choose_provider_at(&config("ollama", false), "http://127.0.0.1:1").unwrap_err();
        assert!(format!("{err:#}").contains("nothing answers"), "{err:#}");
    }

    #[test]
    fn auto_uses_ollama_when_the_model_is_there() {
        let server = ollama::fake::FakeOllama::start(ollama::fake::tags_and_generate(
            r#"{"response":"{\"summary\":\"A model wrote this.\"}"}"#,
        ));
        let mut config = config("auto", false);
        config.llm.model = "qwen3:4b".into();
        let choice = choose_provider_at(&config, server.addr()).unwrap();
        assert!(choice.notices.is_empty(), "{:?}", choice.notices);

        let written = choice.provider.write(&week());
        assert_eq!(written.text, "A model wrote this.");
        assert_eq!(written.generated_by, "ollama:qwen3:4b");
        assert!(written.note.is_none());
    }

    #[test]
    fn auto_falls_back_when_the_model_is_not_pulled() {
        let server = ollama::fake::FakeOllama::start(ollama::fake::tags_and_generate("{}"));
        let mut config = config("auto", false);
        config.llm.model = "mistral".into();
        let choice = choose_provider_at(&config, server.addr()).unwrap();
        assert!(matches!(choice.provider, Provider::Heuristic));
        assert!(
            choice.notices[0].contains("no model `mistral`"),
            "{:?}",
            choice.notices
        );
    }

    #[test]
    fn asking_for_a_model_ollama_does_not_have_is_an_error() {
        let server = ollama::fake::FakeOllama::start(ollama::fake::tags_and_generate("{}"));
        let mut config = config("ollama", false);
        config.llm.model = "mistral".into();
        let err = choose_provider_at(&config, server.addr()).unwrap_err();
        assert!(
            format!("{err:#}").contains("ollama pull mistral"),
            "{err:#}"
        );
    }

    #[test]
    fn a_model_that_answers_rubbish_falls_back_to_the_template() {
        let server = ollama::fake::FakeOllama::start(ollama::fake::tags_and_generate(
            r#"{"response":"here you go!"}"#,
        ));
        let choice = choose_provider_at(&config("auto", false), server.addr()).unwrap();
        let written = choice.provider.write(&week());

        assert_eq!(written.generated_by, "heuristic");
        assert_eq!(written.text, heuristic::summarize(&week()));
        assert!(written
            .note
            .unwrap()
            .contains("did not return a usable summary"));
    }

    fn week() -> Week {
        Week {
            index: 1,
            start: "2026-09-14".parse().unwrap(),
            end: "2026-09-20".parse().unwrap(),
            commits: vec![],
            decisions: vec!["chose sqlite".into()],
            patterns: vec![],
        }
    }

    #[test]
    fn the_default_model_is_used_when_the_config_is_blank() {
        let mut config = config("none", false);
        config.llm.model = "  ".into();
        assert_eq!(model_name(&config), DEFAULT_MODEL);
        config.llm.model = "llama3".into();
        assert_eq!(model_name(&config), "llama3");
    }
}
