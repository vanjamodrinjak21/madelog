//! The terminal half of the preview: ratatui draws, crossterm reads keys.
//!
//! Nothing here decides anything. Keys are turned into [`super::Key`] and
//! handed to [`super::apply`], which is the same code the tests drive, so the
//! screen cannot grow behaviour the tests do not see.
//!
//! Palette: none. The brand is near black and white, and status is carried by
//! weight and by words, never by colour (spec §6).

use std::io::IsTerminal;

use anyhow::{Context, Result};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};

use super::{apply, Host, Item, ItemKind, Key, Model, Outcome, Preview};

/// The real preview screen.
#[derive(Debug, Default)]
pub struct Tui {
    /// How far the detail pane is scrolled, in lines.
    detail_scroll: u16,
}

impl Tui {
    pub fn new() -> Tui {
        Tui::default()
    }
}

impl Preview for Tui {
    /// Without a terminal there is nothing to preview on, and publish goes
    /// straight through (spec §4.5).
    fn is_available(&self) -> bool {
        std::io::stdout().is_terminal() && std::io::stdin().is_terminal()
    }

    fn run(&mut self, model: Model, host: &mut dyn Host) -> Result<Outcome> {
        let mut terminal = ratatui::try_init().context("opening the preview screen")?;
        let outcome = self.loop_until_decided(&mut terminal, model, host);
        // Give the terminal back whatever happened, then report.
        let restored = ratatui::try_restore();
        let outcome = outcome?;
        restored.context("restoring your terminal after the preview")?;
        Ok(outcome)
    }
}

impl Tui {
    fn loop_until_decided(
        &mut self,
        terminal: &mut DefaultTerminal,
        mut model: Model,
        host: &mut dyn Host,
    ) -> Result<Outcome> {
        let mut list_state = ListState::default();
        loop {
            list_state.select(Some(model.selected));
            terminal
                .draw(|frame| self.draw(frame, &model, &mut list_state))
                .context("drawing the preview")?;

            let Some(key) = self.next_key()? else {
                continue;
            };
            match key {
                Press::Scroll(delta) => {
                    self.detail_scroll = self.detail_scroll.saturating_add_signed(delta);
                }
                Press::Model(Key::Edit) => {
                    // $EDITOR needs the real terminal back, and wants it
                    // returned in the state it found it.
                    let target = model
                        .selected_summary()
                        .map(|s| s.id.clone())
                        .filter(|id| !id.is_empty());
                    if let Some(id) = target {
                        ratatui::try_restore().context("leaving the preview for your editor")?;
                        let edited = host.edit_summary(&id);
                        *terminal = ratatui::try_init().context("reopening the preview")?;
                        terminal.clear().ok();
                        // A refused edit is not a reason to lose the preview.
                        if edited.is_ok() {
                            let mut next = host.reload()?;
                            next.restore_cursors(&model);
                            model = next;
                        }
                    }
                }
                Press::Model(k) => {
                    let before = model.selected;
                    if let Some(outcome) = apply(k, &mut model, host)? {
                        return Ok(outcome);
                    }
                    if model.selected != before {
                        self.detail_scroll = 0;
                    }
                }
            }
        }
    }

    /// One key press, or `None` for anything that is not one.
    fn next_key(&self) -> Result<Option<Press>> {
        let Event::Key(key) = event::read().context("reading the keyboard")? else {
            return Ok(None);
        };
        if key.kind != KeyEventKind::Press {
            return Ok(None);
        }
        // Ctrl-C is what people press when they want out.
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('d'))
        {
            return Ok(Some(Press::Model(Key::Abort)));
        }
        Ok(match key.code {
            KeyCode::Char('j') | KeyCode::Down => Some(Press::Model(Key::Down)),
            KeyCode::Char('k') | KeyCode::Up => Some(Press::Model(Key::Up)),
            KeyCode::Tab | KeyCode::Char('n') => Some(Press::Model(Key::NextSummary)),
            KeyCode::BackTab | KeyCode::Char('p') => Some(Press::Model(Key::PrevSummary)),
            KeyCode::Char('h') => Some(Press::Model(Key::Hide)),
            KeyCode::Char('e') => Some(Press::Model(Key::Edit)),
            KeyCode::Enter => Some(Press::Model(Key::Confirm)),
            KeyCode::Char('q') | KeyCode::Esc => Some(Press::Model(Key::Abort)),
            KeyCode::PageDown => Some(Press::Scroll(5)),
            KeyCode::PageUp => Some(Press::Scroll(-5)),
            _ => None,
        })
    }

    fn draw(&self, frame: &mut Frame<'_>, model: &Model, list_state: &mut ListState) {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(6),
                Constraint::Length(9),
                Constraint::Length(1),
            ])
            .split(frame.area());
        let panes = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
            .split(rows[0]);

        self.draw_timeline(frame, panes[0], model, list_state);
        self.draw_detail(frame, panes[1], model);
        self.draw_summaries(frame, rows[1], model);
        self.draw_footer(frame, rows[2], model);
    }

    fn draw_timeline(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        model: &Model,
        state: &mut ListState,
    ) {
        let items: Vec<ListItem<'_>> = model
            .items
            .iter()
            .map(|item| ListItem::new(Line::from(spans_for(item))))
            .collect();
        let list = List::new(items)
            .block(block("timeline"))
            .highlight_symbol("> ")
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
        frame.render_stateful_widget(list, area, state);
    }

    fn draw_detail(&self, frame: &mut Frame<'_>, area: Rect, model: &Model) {
        let lines: Vec<Line<'_>> = match model.selected_item() {
            Some(item) => item.detail.iter().map(|l| Line::raw(l.as_str())).collect(),
            None => vec![Line::raw(
                "Nothing recorded yet. Commit something, or write a note with `madelog note`.",
            )],
        };
        let title = match model.selected_item() {
            Some(item) if item.hidden => "detail — hidden from the page",
            Some(_) => "detail",
            None => "detail",
        };
        let paragraph = Paragraph::new(lines)
            .block(block(title))
            .wrap(Wrap { trim: false })
            .scroll((self.detail_scroll, 0));
        frame.render_widget(paragraph, area);
    }

    fn draw_summaries(&self, frame: &mut Frame<'_>, area: Rect, model: &Model) {
        let mut lines = Vec::new();
        match model.selected_summary() {
            Some(summary) => {
                lines.push(Line::from(vec![
                    Span::styled(
                        summary.heading(),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(format!(
                        "   {} of {}",
                        model.summary_selected + 1,
                        model.summaries.len()
                    )),
                ]));
                lines.extend(summary.text.lines().map(Line::raw));
            }
            None => lines.push(Line::raw(
                "No summaries yet. `madelog summarize` writes one per week.",
            )),
        }
        frame.render_widget(
            Paragraph::new(lines)
                .block(block("summaries"))
                .wrap(Wrap { trim: false }),
            area,
        );
    }

    fn draw_footer(&self, frame: &mut Frame<'_>, area: Rect, model: &Model) {
        let keys = "j/k move   h hide session   e edit summary   Tab next summary   \
                    Enter publish   q abort";
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::raw(keys),
                Span::raw("   ·   "),
                Span::styled(
                    model.status_line(),
                    Style::default().add_modifier(Modifier::DIM),
                ),
            ])),
            area,
        );
    }
}

/// What a key press means: something the model handles, or a pane scroll.
enum Press {
    Model(Key),
    Scroll(i16),
}

fn block(title: &str) -> Block<'_> {
    Block::default().borders(Borders::ALL).title(title)
}

/// A hidden row says so in words, because the page has no colour either.
fn spans_for(item: &Item) -> Vec<Span<'_>> {
    let mut spans = vec![Span::raw(super::day(&item.ts)), Span::raw("  ")];
    let kind = format!("{:<9}", item.kind.label());
    spans.push(match item.kind {
        ItemKind::AiSession => Span::styled(kind, Style::default().add_modifier(Modifier::ITALIC)),
        _ => Span::raw(kind),
    });
    if item.hidden {
        spans.push(Span::styled(
            "[hidden] ",
            Style::default().add_modifier(Modifier::BOLD),
        ));
    }
    spans.push(Span::raw(item.title.as_str()));
    spans
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn model() -> Model {
        let mut model = Model {
            project: "madelog".into(),
            items: vec![
                Item {
                    id: "1".into(),
                    kind: ItemKind::Commit,
                    ts: "2026-09-17T14:02:00.000Z".into(),
                    title: "feat: add the store".into(),
                    detail: vec!["commit abc1234".into(), "src/store.rs".into()],
                    hidden: false,
                },
                Item {
                    id: "2".into(),
                    kind: ItemKind::AiSession,
                    ts: "2026-09-17T14:20:00.000Z".into(),
                    title: "claude-code: write the store".into(),
                    detail: vec!["claude-code sonnet".into()],
                    hidden: true,
                },
            ],
            summaries: vec![super::super::Summary {
                id: "s".into(),
                week_start: "2026-09-14".into(),
                week_end: "2026-09-20".into(),
                text: "You built the store.".into(),
                generated_by: "heuristic".into(),
                edited: false,
            }],
            hidden_sessions: 1,
            event_count: 20,
            selected: 0,
            summary_selected: 0,
        };
        model.selected = 0;
        model
    }

    /// The whole screen, rendered to a buffer, as one string per row.
    fn screen(model: &Model) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(160, 26)).unwrap();
        let tui = Tui::new();
        let mut state = ListState::default();
        state.select(Some(model.selected));
        terminal
            .draw(|frame| tui.draw(frame, model, &mut state))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn the_screen_has_three_panes_and_a_key_line() {
        let text = screen(&model()).join("\n");
        assert!(text.contains("timeline"), "{text}");
        assert!(text.contains("detail"), "{text}");
        assert!(text.contains("summaries"), "{text}");
        assert!(text.contains("Enter publish"), "{text}");
        assert!(text.contains("q abort"), "{text}");
    }

    #[test]
    fn the_timeline_marks_a_hidden_session_in_words() {
        let text = screen(&model()).join("\n");
        assert!(text.contains("[hidden]"), "{text}");
        assert!(text.contains("claude-code: write the store"), "{text}");
        assert!(text.contains("1 session hidden"), "{text}");
    }

    #[test]
    fn the_detail_pane_follows_the_cursor() {
        let mut model = model();
        assert!(screen(&model).join("\n").contains("commit abc1234"));
        model.selected = 1;
        let text = screen(&model).join("\n");
        assert!(text.contains("claude-code sonnet"), "{text}");
        assert!(text.contains("hidden from the page"), "{text}");
    }

    #[test]
    fn the_summary_pane_shows_the_selected_week() {
        let text = screen(&model()).join("\n");
        assert!(text.contains("2026-09-14 to 2026-09-20"), "{text}");
        assert!(text.contains("You built the store."), "{text}");
        assert!(text.contains("1 of 1"), "{text}");
    }

    #[test]
    fn an_empty_log_still_draws() {
        let empty = Model {
            project: "madelog".into(),
            ..Default::default()
        };
        let text = screen(&empty).join("\n");
        assert!(text.contains("Nothing recorded yet"), "{text}");
        assert!(text.contains("No summaries yet"), "{text}");
    }
}
