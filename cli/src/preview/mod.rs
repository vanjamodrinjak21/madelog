//! The publish preview (spec §4.5): one screen, three panes, four keys.
//!
//! The terminal lives in [`tui`]. Everything else — what the screen shows and
//! what the keys are allowed to change — is plain data behind two traits, so
//! the confirm and abort paths can be tested without a terminal:
//!
//! - [`Model`] is the screen's contents, rebuilt from the log.
//! - [`Host`] is the only way the preview may touch the log (hide a session,
//!   edit a summary, reload).
//! - [`Preview`] is the screen itself; [`Scripted`] is one that answers from a
//!   list of key presses.

pub mod tui;

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::event::{Event, EventKind, SummaryPayload};

/// Below this many events the project is too thin to say much (spec §9).
pub const THIN_EVENT_COUNT: usize = 15;

/// What the person decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Enter: publish.
    Confirm,
    /// q: stop, change nothing more.
    Abort,
}

// ---------------------------------------------------------------------------
// the model
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    Commit,
    Decision,
    AiSession,
}

impl ItemKind {
    pub fn label(&self) -> &'static str {
        match self {
            ItemKind::Commit => "commit",
            ItemKind::Decision => "decision",
            ItemKind::AiSession => "session",
        }
    }
}

/// One row of the timeline, and everything the detail pane shows for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub id: String,
    pub kind: ItemKind,
    pub ts: String,
    /// One line for the list.
    pub title: String,
    /// The detail pane, already wrapped into labelled lines.
    pub detail: Vec<String>,
    /// True for an AI session someone has already hidden.
    pub hidden: bool,
}

impl Item {
    /// The left pane's line: date, kind, marker, title.
    pub fn line(&self) -> String {
        let mark = if self.hidden { " [hidden]" } else { "" };
        format!(
            "{}  {:<8}{}  {}",
            day(&self.ts),
            self.kind.label(),
            mark,
            self.title
        )
    }
}

/// One weekly summary, latest version only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub id: String,
    pub week_start: String,
    pub week_end: String,
    pub text: String,
    pub generated_by: String,
    pub edited: bool,
}

impl Summary {
    pub fn heading(&self) -> String {
        let how = if self.edited {
            "edited by you".to_string()
        } else {
            format!("generated, {}", self.generated_by)
        };
        format!("{} to {}  ({how})", self.week_start, self.week_end)
    }
}

/// Everything on the screen.
#[derive(Debug, Clone, Default)]
pub struct Model {
    pub project: String,
    pub items: Vec<Item>,
    pub summaries: Vec<Summary>,
    /// How many AI sessions are hidden right now.
    pub hidden_sessions: usize,
    /// Every event in the log, hidden ones included.
    pub event_count: usize,
    /// Which timeline row the cursor is on.
    pub selected: usize,
    /// Which summary `e` would edit.
    pub summary_selected: usize,
}

impl Model {
    /// Build the screen from the log at `<root>/.madelog/log.db`.
    pub fn build(root: &Path) -> Result<Model> {
        let store = crate::store::Store::open(&root.join(".madelog").join("log.db"))?;
        let events = store.all()?;
        Ok(Model::from_events(
            &root
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "this project".to_string()),
            &events,
        ))
    }

    pub fn from_events(project: &str, events: &[Event]) -> Model {
        let hidden = hidden_ids(events);
        let mut items = Vec::new();
        for event in events {
            let Some(kind) = item_kind(event.kind) else {
                continue;
            };
            // A private decision never reaches the page, so it never reaches
            // the preview of the page either.
            if kind == ItemKind::Decision && is_private(event) {
                continue;
            }
            items.push(Item {
                id: event.id.clone(),
                kind,
                ts: event.ts.clone(),
                title: event.title(),
                detail: detail_lines(event),
                hidden: hidden.contains(&event.id),
            });
        }

        let summaries: Vec<Summary> = crate::summarize::latest_summaries(events)
            .into_iter()
            .map(|s| summary_of(events, s))
            .collect();

        let hidden_sessions = items.iter().filter(|i| i.hidden).count();
        Model {
            project: project.to_string(),
            items,
            summaries,
            hidden_sessions,
            event_count: events.len(),
            selected: 0,
            summary_selected: 0,
        }
    }

    pub fn selected_item(&self) -> Option<&Item> {
        self.items.get(self.selected)
    }

    pub fn selected_summary(&self) -> Option<&Summary> {
        self.summaries.get(self.summary_selected)
    }

    /// Move the timeline cursor, clamped at both ends.
    pub fn move_by(&mut self, delta: isize) {
        if self.items.is_empty() {
            self.selected = 0;
            return;
        }
        let last = self.items.len() as isize - 1;
        let next = (self.selected as isize + delta).clamp(0, last);
        self.selected = next as usize;
    }

    /// Keep the cursors pointing at something after a reload.
    pub fn restore_cursors(&mut self, from: &Model) {
        self.selected = from.selected.min(self.items.len().saturating_sub(1));
        self.summary_selected = from
            .summary_selected
            .min(self.summaries.len().saturating_sub(1));
    }

    /// The warning for a project with almost nothing in it. Warns, never blocks.
    pub fn thin_warning(&self) -> Option<String> {
        thin_warning(self.event_count)
    }

    /// The one-line footer under the three panes.
    pub fn status_line(&self) -> String {
        let hidden = match self.hidden_sessions {
            0 => String::new(),
            n => format!("  ·  {} hidden", plural(n, "session")),
        };
        format!(
            "{}  ·  {}  ·  {}{hidden}",
            self.project,
            plural(self.items.len(), "entry"),
            plural(self.summaries.len(), "summary"),
        )
    }
}

/// Fewer than [`THIN_EVENT_COUNT`] events makes a thin page. Spec §9 says warn
/// and publish anyway, so this returns words, never an error.
pub fn thin_warning(event_count: usize) -> Option<String> {
    if event_count >= THIN_EVENT_COUNT {
        return None;
    }
    Some(format!(
        "this project has {} so far, and a page built from fewer than {THIN_EVENT_COUNT} events \
         says very little. It will still publish. Keep committing and run `madelog publish` again later.",
        plural(event_count, "event"),
    ))
}

fn plural(n: usize, noun: &str) -> String {
    let plural = match noun {
        "entry" => "entries".to_string(),
        "summary" => "summaries".to_string(),
        other => format!("{other}s"),
    };
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {plural}")
    }
}

fn item_kind(kind: EventKind) -> Option<ItemKind> {
    match kind {
        EventKind::Commit => Some(ItemKind::Commit),
        EventKind::Decision => Some(ItemKind::Decision),
        EventKind::AiSession => Some(ItemKind::AiSession),
        _ => None,
    }
}

fn is_private(event: &Event) -> bool {
    event
        .payload
        .get("private")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

fn hidden_ids(events: &[Event]) -> std::collections::BTreeSet<String> {
    events
        .iter()
        .filter(|e| e.kind == EventKind::Hide)
        .filter_map(|e| {
            e.payload
                .get("target_id")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .collect()
}

/// The latest summary event for a week, so `e` edits the one the page shows.
fn summary_of(events: &[Event], payload: SummaryPayload) -> Summary {
    let id = events
        .iter()
        .rev()
        .find(|e| {
            e.kind == EventKind::Summary
                && e.payload.get("week_start").and_then(|v| v.as_str())
                    == Some(payload.week_start.as_str())
        })
        .map(|e| e.id.clone())
        .unwrap_or_default();
    Summary {
        id,
        week_start: payload.week_start,
        week_end: payload.week_end,
        text: payload.text,
        generated_by: payload.generated_by,
        edited: payload.edited,
    }
}

/// `2026-09-17T14:02:00.123Z` reads better as `2026-09-17 14:02`.
fn day(ts: &str) -> String {
    let (date, rest) = match ts.split_once('T') {
        Some(parts) => parts,
        None => return ts.to_string(),
    };
    let time: String = rest.chars().take(5).collect();
    format!("{date} {time}")
}

/// The right pane: the commit's message and files, the decision's text, or the
/// session's tool, model and prompt.
fn detail_lines(event: &Event) -> Vec<String> {
    let get = |key: &str| {
        event
            .payload
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    let mut out = Vec::new();
    match event.kind {
        EventKind::Commit => {
            let sha = get("sha");
            out.push(format!("commit {}", &sha[..7.min(sha.len())]));
            out.push(format!("{}  {}", day(&event.ts), get("author")));
            out.push(String::new());
            out.extend(get("message").lines().map(str::to_string));
            let files = event
                .payload
                .get("files")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            if !files.is_empty() {
                out.push(String::new());
                out.push(format!("{}:", plural(files.len(), "file")));
                for file in files.iter().take(20) {
                    let path = file.get("path").and_then(|v| v.as_str()).unwrap_or("?");
                    let added = file.get("added").and_then(|v| v.as_u64()).unwrap_or(0);
                    let removed = file.get("removed").and_then(|v| v.as_u64()).unwrap_or(0);
                    out.push(format!("  +{added:<5} -{removed:<5} {path}"));
                }
                if files.len() > 20 {
                    out.push(format!("  and {} more", files.len() - 20));
                }
            }
        }
        EventKind::Decision => {
            out.push(format!("decision  {}", day(&event.ts)));
            out.push(String::new());
            out.extend(get("text").lines().map(str::to_string));
        }
        EventKind::AiSession => {
            let model = event
                .payload
                .get("model")
                .and_then(|v| v.as_str())
                .unwrap_or("model not recorded");
            out.push(format!("{}  {model}", get("tool")));
            out.push(format!("{}  session {}", day(&event.ts), get("session_id")));
            out.push(String::new());
            out.push("prompt:".to_string());
            match event.payload.get("prompt").and_then(|v| v.as_str()) {
                Some(prompt) if !prompt.trim().is_empty() => {
                    out.extend(prompt.lines().map(str::to_string))
                }
                _ => out.push("  (not recorded)".to_string()),
            }
            let files = event
                .payload
                .get("files")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            if !files.is_empty() {
                out.push(String::new());
                out.push("attributed lines:".to_string());
                for file in files.iter().take(20) {
                    let path = file.get("path").and_then(|v| v.as_str()).unwrap_or("?");
                    let ai = file.get("ai_lines").and_then(|v| v.as_u64()).unwrap_or(0);
                    let reworked = file
                        .get("ai_lines_modified_by_human")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    out.push(format!("  {ai:<5} ai  {reworked:<5} reworked  {path}"));
                }
            }
        }
        _ => out.push(event.title()),
    }
    out
}

// ---------------------------------------------------------------------------
// the host
// ---------------------------------------------------------------------------

/// The only things the preview may do to the log. Implemented once for real,
/// once in the tests.
pub trait Host {
    /// `h`: append a `hide` event for this AI session.
    fn hide_session(&mut self, event_id: &str) -> Result<()>;
    /// `e`: open this summary in `$EDITOR` and append what comes back.
    fn edit_summary(&mut self, event_id: &str) -> Result<()>;
    /// Rebuild the model after either of the above.
    fn reload(&mut self) -> Result<Model>;
}

/// The real host: `madelog hide` and `madelog summary edit`, exactly as the commands
/// run them, with their output swallowed so it cannot land on the screen.
pub struct LogHost {
    root: PathBuf,
    /// Whatever the reused commands printed, kept for after the screen closes.
    pub said: Vec<String>,
}

impl LogHost {
    pub fn new(root: &Path) -> LogHost {
        LogHost {
            root: root.to_path_buf(),
            said: Vec::new(),
        }
    }

    fn run_command(&mut self, f: impl FnOnce(&mut crate::Ctx<'_>) -> Result<()>) -> Result<()> {
        let mut sink: Vec<u8> = Vec::new();
        let stdin = std::io::stdin();
        let mut input = stdin.lock();
        let mut ctx = crate::Ctx {
            cwd: self.root.clone(),
            input: &mut input,
            out: &mut sink,
        };
        let outcome = f(&mut ctx);
        let said = String::from_utf8_lossy(&sink).into_owned();
        for line in said.lines() {
            if !line.trim().is_empty() {
                self.said.push(line.to_string());
            }
        }
        outcome
    }
}

impl Host for LogHost {
    fn hide_session(&mut self, event_id: &str) -> Result<()> {
        let id = event_id.to_string();
        self.run_command(|ctx| crate::commands::hide::run(ctx, &id))
    }

    fn edit_summary(&mut self, event_id: &str) -> Result<()> {
        let id = event_id.to_string();
        self.run_command(|ctx| crate::commands::summarize::edit(ctx, &id))
    }

    fn reload(&mut self) -> Result<Model> {
        Model::build(&self.root)
    }
}

// ---------------------------------------------------------------------------
// the screen
// ---------------------------------------------------------------------------

/// One key press, named rather than typed, so a test can send one without
/// crossterm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Down,
    Up,
    NextSummary,
    PrevSummary,
    Hide,
    Edit,
    Confirm,
    Abort,
}

/// Apply one key to the model. The screen is only a renderer; this is the
/// behaviour, and it is the same for the terminal and for the tests.
pub fn apply(key: Key, model: &mut Model, host: &mut dyn Host) -> Result<Option<Outcome>> {
    match key {
        Key::Down => model.move_by(1),
        Key::Up => model.move_by(-1),
        Key::NextSummary => {
            if !model.summaries.is_empty() {
                model.summary_selected = (model.summary_selected + 1) % model.summaries.len();
            }
        }
        Key::PrevSummary => {
            if !model.summaries.is_empty() {
                model.summary_selected = model
                    .summary_selected
                    .checked_sub(1)
                    .unwrap_or(model.summaries.len() - 1);
            }
        }
        Key::Hide => {
            let target = model
                .selected_item()
                .filter(|i| i.kind == ItemKind::AiSession && !i.hidden)
                .map(|i| i.id.clone());
            if let Some(id) = target {
                host.hide_session(&id)?;
                let mut next = host.reload()?;
                next.restore_cursors(model);
                *model = next;
            }
        }
        Key::Edit => {
            let target = model
                .selected_summary()
                .map(|s| s.id.clone())
                .filter(|id| !id.is_empty());
            if let Some(id) = target {
                host.edit_summary(&id)?;
                let mut next = host.reload()?;
                next.restore_cursors(model);
                *model = next;
            }
        }
        Key::Confirm => return Ok(Some(Outcome::Confirm)),
        Key::Abort => return Ok(Some(Outcome::Abort)),
    }
    Ok(None)
}

/// The preview screen. [`tui::Tui`] draws it; [`Scripted`] pretends to.
pub trait Preview {
    /// False when this screen cannot run here — no terminal, for instance.
    fn is_available(&self) -> bool;
    /// Show the model until the person confirms or aborts.
    fn run(&mut self, model: Model, host: &mut dyn Host) -> Result<Outcome>;
}

/// A preview that answers from a list of key presses instead of a terminal.
/// This is how the tests walk the confirm and abort paths.
pub struct Scripted {
    keys: Vec<Key>,
    /// What the model looked like when the screen closed.
    pub last: Option<Model>,
    /// Set false to make publish behave as if there were no terminal.
    pub available: bool,
}

impl Scripted {
    /// The keys are pressed in order; the run ends at the first Enter or q.
    pub fn new(keys: &[Key]) -> Scripted {
        Scripted {
            keys: keys.to_vec(),
            last: None,
            available: true,
        }
    }

    /// A preview that publishes without touching anything.
    pub fn confirm() -> Scripted {
        Scripted::new(&[Key::Confirm])
    }

    /// A preview that gives up.
    pub fn abort() -> Scripted {
        Scripted::new(&[Key::Abort])
    }

    /// A preview that never opens, the way it behaves without a terminal.
    pub fn unavailable() -> Scripted {
        let mut s = Scripted::confirm();
        s.available = false;
        s
    }
}

impl Preview for Scripted {
    fn is_available(&self) -> bool {
        self.available
    }

    fn run(&mut self, model: Model, host: &mut dyn Host) -> Result<Outcome> {
        let mut model = model;
        let mut outcome = Outcome::Abort;
        for key in std::mem::take(&mut self.keys) {
            if let Some(decided) = apply(key, &mut model, host)? {
                outcome = decided;
                break;
            }
        }
        self.last = Some(model);
        Ok(outcome)
    }
}

/// The preview `madelog publish` uses: the real terminal one.
pub fn default_preview() -> Box<dyn Preview> {
    Box::new(tui::Tui::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{
        AiSessionFile, AiSessionPayload, CommitFile, CommitPayload, DecisionPayload, FileStatus,
        HidePayload, Payload, SummaryPayload,
    };
    use crate::store::Store;

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::TempDir::new().unwrap();
        let store = Store::open(&dir.path().join(".madelog").join("log.db")).unwrap();
        (dir, store)
    }

    fn session(store: &Store, prompt: &str) -> String {
        store
            .append_payload(
                None,
                &Payload::AiSession(AiSessionPayload {
                    tool: "claude-code".into(),
                    model: Some("sonnet".into()),
                    session_id: "s-1".into(),
                    prompt: Some(prompt.into()),
                    response_excerpt: None,
                    files: vec![AiSessionFile {
                        path: "src/store.rs".into(),
                        ai_lines: 40,
                        ai_lines_modified_by_human: 12,
                    }],
                    accepted: true,
                }),
            )
            .unwrap()
            .id
    }

    fn populated() -> (tempfile::TempDir, Store) {
        let (dir, store) = store();
        store
            .append_payload(
                Some(&"a".repeat(40)),
                &Payload::Commit(CommitPayload {
                    sha: "a".repeat(40),
                    message: "feat: add the store".into(),
                    author: "Test Person".into(),
                    parents: vec![],
                    files: vec![CommitFile {
                        path: "src/store.rs".into(),
                        added: 120,
                        removed: 3,
                        status: FileStatus::Added,
                        diff_excerpt: None,
                    }],
                    secs_since_prev: None,
                }),
            )
            .unwrap();
        store
            .append_payload(
                None,
                &Payload::Decision(DecisionPayload {
                    text: "chose sqlite over a file per event".into(),
                    private: false,
                }),
            )
            .unwrap();
        store
            .append_payload(
                None,
                &Payload::Decision(DecisionPayload {
                    text: "this one is nobody's business".into(),
                    private: true,
                }),
            )
            .unwrap();
        session(&store, "write an append-only store");
        store
            .append_payload(
                None,
                &Payload::Summary(SummaryPayload {
                    week_start: "2026-09-14".into(),
                    week_end: "2026-09-20".into(),
                    text: "You built the store.".into(),
                    generated_by: "heuristic".into(),
                    edited: false,
                }),
            )
            .unwrap();
        (dir, store)
    }

    /// A host that records what it was asked to do and hides for real.
    struct FakeHost {
        store: Store,
        pub hidden: Vec<String>,
        pub edited: Vec<String>,
    }

    impl Host for FakeHost {
        fn hide_session(&mut self, event_id: &str) -> Result<()> {
            self.hidden.push(event_id.to_string());
            self.store.append_payload(
                None,
                &Payload::Hide(HidePayload {
                    target_kind: "ai_session".into(),
                    target_id: event_id.to_string(),
                }),
            )?;
            Ok(())
        }
        fn edit_summary(&mut self, event_id: &str) -> Result<()> {
            self.edited.push(event_id.to_string());
            Ok(())
        }
        fn reload(&mut self) -> Result<Model> {
            Ok(Model::from_events("madelog", &self.store.all()?))
        }
    }

    #[test]
    fn the_timeline_holds_commits_decisions_and_sessions_but_not_private_notes() {
        let (_d, store) = populated();
        let model = Model::from_events("madelog", &store.all().unwrap());
        let kinds: Vec<ItemKind> = model.items.iter().map(|i| i.kind).collect();
        assert_eq!(
            kinds,
            vec![ItemKind::Commit, ItemKind::Decision, ItemKind::AiSession]
        );
        assert!(
            !model
                .items
                .iter()
                .any(|i| i.title.contains("nobody's business")),
            "private decisions never render"
        );
        assert_eq!(model.summaries.len(), 1);
        assert_eq!(model.hidden_sessions, 0);
    }

    #[test]
    fn each_kind_gets_the_detail_the_spec_asks_for() {
        let (_d, store) = populated();
        let model = Model::from_events("madelog", &store.all().unwrap());
        let commit = model.items[0].detail.join("\n");
        assert!(commit.contains("feat: add the store"), "{commit}");
        assert!(commit.contains("src/store.rs"), "{commit}");
        assert!(commit.contains("+120"), "{commit}");

        let decision = model.items[1].detail.join("\n");
        assert!(decision.contains("chose sqlite"), "{decision}");

        let ai = model.items[2].detail.join("\n");
        assert!(ai.contains("claude-code"), "{ai}");
        assert!(ai.contains("sonnet"), "{ai}");
        assert!(ai.contains("write an append-only store"), "{ai}");
    }

    #[test]
    fn j_and_k_move_the_cursor_and_stop_at_the_ends() {
        let (_d, store) = populated();
        let mut model = Model::from_events("madelog", &store.all().unwrap());
        let mut host = FakeHost {
            store: Store::open(store.path()).unwrap(),
            hidden: vec![],
            edited: vec![],
        };
        assert_eq!(model.selected, 0);
        apply(Key::Up, &mut model, &mut host).unwrap();
        assert_eq!(model.selected, 0, "the top does not wrap");
        for _ in 0..10 {
            apply(Key::Down, &mut model, &mut host).unwrap();
        }
        assert_eq!(model.selected, model.items.len() - 1, "the bottom holds");
    }

    #[test]
    fn h_hides_the_selected_session_and_the_screen_shows_it_at_once() {
        let (_d, store) = populated();
        let mut model = Model::from_events("madelog", &store.all().unwrap());
        let mut host = FakeHost {
            store: Store::open(store.path()).unwrap(),
            hidden: vec![],
            edited: vec![],
        };

        // On a commit, `h` does nothing at all.
        model.selected = 0;
        apply(Key::Hide, &mut model, &mut host).unwrap();
        assert!(host.hidden.is_empty());

        model.selected = 2;
        let session_id = model.items[2].id.clone();
        apply(Key::Hide, &mut model, &mut host).unwrap();
        assert_eq!(host.hidden, vec![session_id]);
        assert!(model.items[2].hidden, "the row is marked straight away");
        assert_eq!(model.hidden_sessions, 1);
        assert!(model.items[2].line().contains("[hidden]"));
        assert!(model.status_line().contains("1 session hidden"));

        // Hiding it twice is not a thing.
        apply(Key::Hide, &mut model, &mut host).unwrap();
        assert_eq!(host.hidden.len(), 1);
    }

    #[test]
    fn e_edits_the_selected_summary() {
        let (_d, store) = populated();
        let mut model = Model::from_events("madelog", &store.all().unwrap());
        let mut host = FakeHost {
            store: Store::open(store.path()).unwrap(),
            hidden: vec![],
            edited: vec![],
        };
        let summary_id = model.summaries[0].id.clone();
        assert!(!summary_id.is_empty());
        apply(Key::Edit, &mut model, &mut host).unwrap();
        assert_eq!(host.edited, vec![summary_id]);
    }

    #[test]
    fn enter_confirms_and_q_aborts() {
        let (_d, store) = populated();
        let model = Model::from_events("madelog", &store.all().unwrap());
        let mut host = FakeHost {
            store: Store::open(store.path()).unwrap(),
            hidden: vec![],
            edited: vec![],
        };
        assert_eq!(
            Scripted::confirm().run(model.clone(), &mut host).unwrap(),
            Outcome::Confirm
        );
        assert_eq!(
            Scripted::abort().run(model, &mut host).unwrap(),
            Outcome::Abort
        );
    }

    #[test]
    fn a_thin_project_is_warned_about_not_blocked() {
        let (_d, store) = populated();
        let model = Model::from_events("madelog", &store.all().unwrap());
        let warning = model.thin_warning().expect("five events is thin");
        assert!(warning.contains("5 events"), "{warning}");
        assert!(warning.contains("15"), "{warning}");
        assert!(warning.contains("still publish"), "{warning}");

        let mut fat = model;
        fat.event_count = THIN_EVENT_COUNT;
        assert_eq!(fat.thin_warning(), None);
    }

    #[test]
    fn a_summary_heading_says_who_wrote_it() {
        let generated = Summary {
            id: "a".into(),
            week_start: "2026-09-14".into(),
            week_end: "2026-09-20".into(),
            text: "x".into(),
            generated_by: "ollama:qwen3:4b".into(),
            edited: false,
        };
        assert!(generated.heading().contains("generated, ollama:qwen3:4b"));
        let edited = Summary {
            edited: true,
            generated_by: "user".into(),
            ..generated
        };
        assert!(edited.heading().contains("edited by you"));
    }
}
