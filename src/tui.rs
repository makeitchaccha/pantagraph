//! Interactive TUI for browsing Pantagraph search results.
//!
//! A thin presentation layer over the existing search pipeline:
//!
//! ```text
//! query input → query parser → tag resolution → paged store search → results
//! ```
//!
//! The first version is deliberately small: query → 10 results →
//! navigate → select → repeat.
//!
//! Tag references resolve exactly like the CLI. In addition to the CLI's
//! text error, an ambiguous reference opens a selection list, and while
//! typing, matching tags are suggested for completion.

use std::collections::HashMap;

use anyhow::Result;
use pantagraph::domain::{Entry, Tag, TagId};
use pantagraph::store::JsonlStore;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::prelude::*;
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph};

use crate::application::entry::{self, EntryPage};
use crate::application::tag::{self, TagAmbiguity, TagResult};

const QUERY_PREFIX: &str = "Query: ";
/// Characters the query parser treats as reserved: they end a tag reference.
const QUERY_RESERVED: [char; 5] = ['+', '*', '!', '(', ')'];
/// Maximum number of tag suggestions shown while typing.
const SUGGESTION_LIMIT: usize = 8;
/// Maximum rows the candidate chooser occupies before scrolling internally.
const CHOOSER_MAX_ROWS: usize = 10;

#[derive(Debug, Clone)]
enum Mode {
    /// Navigate the results: move the selection, change page, quit.
    Normal,
    /// Edit the query.
    Insert,
    /// A tag reference matched several tags: pick one.
    Pick {
        /// The ambiguous reference exactly as typed in the query.
        input: String,
        /// Matching tags in `#N` order.
        candidates: Vec<Tag>,
        selected: usize,
    },
}

#[derive(Debug)]
struct App {
    query: String,
    mode: Mode,
    /// Zero-based result page.
    page: usize,
    /// Selection index within the current page.
    selected: usize,
    entries: Vec<Entry>,
    names: HashMap<TagId, String>,
    has_more: bool,
    error: Option<String>,
    /// Tag completions for the word currently being typed (Insert mode).
    suggestions: Vec<Tag>,
    /// Selection index within `suggestions`.
    suggestion: usize,
}

impl App {
    fn new() -> Self {
        Self {
            query: String::new(),
            mode: Mode::Normal,
            page: 0,
            selected: 0,
            entries: Vec::new(),
            names: HashMap::new(),
            has_more: false,
            error: None,
            suggestions: Vec::new(),
            suggestion: 0,
        }
    }

    /// Run the current query from the first page.
    ///
    /// Success resets the page and selection; failure keeps the previous
    /// results and reports the error inside the TUI. An ambiguous tag
    /// reference opens the candidate picker instead of an error line.
    fn execute(&mut self, store: &JsonlStore) {
        match entry::handle_entry_search_page(store, &self.query, 0) {
            Ok(page) => {
                self.page = 0;
                self.selected = 0;
                self.apply(page);
                self.suggestions.clear();
                self.suggestion = 0;
                self.mode = Mode::Normal;
            }
            Err(e) => match e.downcast::<TagAmbiguity>() {
                Ok(amb) => {
                    self.error = None;
                    self.suggestions.clear();
                    self.suggestion = 0;
                    self.mode = Mode::Pick {
                        input: amb.input,
                        candidates: amb.candidates,
                        selected: 0,
                    };
                }
                Err(e) => self.error = Some(format!("{e:#}")),
            },
        }
    }

    fn load_page(&mut self, store: &JsonlStore) {
        match entry::handle_entry_search_page(store, &self.query, self.page) {
            Ok(page) => self.apply(page),
            Err(e) => self.error = Some(format!("{e:#}")),
        }
    }

    fn apply(&mut self, page: EntryPage) {
        self.entries = page.entries;
        self.names = page.names;
        self.has_more = page.has_more;
        self.error = None;
    }

    fn next_page(&mut self, store: &JsonlStore) {
        if !self.has_more {
            return;
        }
        self.page += 1;
        self.selected = 0;
        self.load_page(store);
    }

    fn prev_page(&mut self, store: &JsonlStore) {
        if self.page == 0 {
            return;
        }
        self.page -= 1;
        self.selected = 0;
        self.load_page(store);
    }

    fn select_next(&mut self) {
        if self.selected + 1 < self.entries.len() {
            self.selected += 1;
        }
    }

    fn select_prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    /// Re-query tag suggestions for the word currently being typed.
    fn refresh_suggestions(&mut self, store: &JsonlStore) {
        self.suggestions.clear();
        self.suggestion = 0;
        let word = word_at_end(&self.query).to_string();
        if word.is_empty() {
            return;
        }
        if let Ok(TagResult::Listed(mut tags)) = tag::handle_tag_list(store, Some(word), None, None) {
            tags.truncate(SUGGESTION_LIMIT);
            self.suggestions = tags;
        }
    }

    fn suggestion_move(&mut self, delta: i32) {
        if self.suggestions.is_empty() {
            return;
        }
        self.suggestion = move_index(self.suggestion, delta, self.suggestions.len());
    }

    /// Replace the word being typed with the selected suggestion.
    fn accept_suggestion(&mut self, store: &JsonlStore) {
        let Some(tag) = self.suggestions.get(self.suggestion) else {
            return;
        };
        let full = tag.display_name();
        let word = word_at_end(&self.query).to_string();
        if word.is_empty() {
            return;
        }
        self.query = replace_word(&self.query, &word, &full);
        self.refresh_suggestions(store);
    }

    fn pick_move(&mut self, delta: i32) {
        if let Mode::Pick {
            candidates,
            selected,
            ..
        } = &mut self.mode
        {
            *selected = move_index(*selected, delta, candidates.len().max(1));
        }
    }

    fn entry_tags(&self, entry: &Entry) -> String {
        if entry.tag_ids.is_empty() {
            return "(no tags)".to_string();
        }
        entry
            .tag_ids
            .iter()
            .map(|id| self.names.get(id).map(String::as_str).unwrap_or("???"))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn move_index(current: usize, delta: i32, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    let last = len - 1;
    if delta < 0 {
        current.saturating_sub((-delta) as usize)
    } else {
        (current + delta as usize).min(last)
    }
}

/// The word the cursor sits in: the trailing run of characters that the
/// query parser would treat as a single tag reference.
fn word_at_end(query: &str) -> &str {
    query
        .rsplit(|c: char| QUERY_RESERVED.contains(&c) || c.is_whitespace())
        .next()
        .unwrap_or("")
}

/// Replace every standalone occurrence of `word` with `replacement`.
///
/// Standalone means the whole token, bounded by whitespace or the query's
/// reserved characters, so `rust` does not match inside `rustacean`.
fn replace_word(query: &str, word: &str, replacement: &str) -> String {
    if word.is_empty() {
        return query.to_string();
    }
    let mut out = String::with_capacity(query.len());
    let mut token = String::new();
    for c in query.chars() {
        if QUERY_RESERVED.contains(&c) || c.is_whitespace() {
            if token == word {
                out.push_str(replacement);
            } else {
                out.push_str(&token);
            }
            token.clear();
            out.push(c);
        } else {
            token.push(c);
        }
    }
    if token == word {
        out.push_str(replacement);
    } else {
        out.push_str(&token);
    }
    out
}

/// Launch the interactive TUI on the given store.
pub fn run(store: &JsonlStore) -> Result<()> {
    let mut terminal = ratatui::init();
    let mut app = App::new();
    app.execute(store);
    let result = event_loop(&mut terminal, store, &mut app);
    ratatui::restore();
    result
}

fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    store: &JsonlStore,
    app: &mut App,
) -> Result<()> {
    loop {
        terminal.draw(|frame| draw(frame, app))?;
        let flow = match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => handle_key(app, store, key),
            _ => Flow::Continue,
        };
        if flow == Flow::Quit {
            return Ok(());
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    Continue,
    Quit,
}

fn handle_key(app: &mut App, store: &JsonlStore, key: KeyEvent) -> Flow {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match app.mode.clone() {
        Mode::Normal => match key.code {
            KeyCode::Esc => Flow::Quit,
            KeyCode::Char('q') if !ctrl => Flow::Quit,
            KeyCode::Char('c') if ctrl => Flow::Quit,
            KeyCode::Down | KeyCode::Char('j') if !ctrl => {
                app.select_next();
                Flow::Continue
            }
            KeyCode::Up | KeyCode::Char('k') if !ctrl => {
                app.select_prev();
                Flow::Continue
            }
            KeyCode::Char('n') if !ctrl => {
                app.next_page(store);
                Flow::Continue
            }
            KeyCode::Char('p') if !ctrl => {
                app.prev_page(store);
                Flow::Continue
            }
            KeyCode::Enter => {
                app.mode = Mode::Insert;
                app.refresh_suggestions(store);
                Flow::Continue
            }
            KeyCode::Char('i') if !ctrl => {
                app.mode = Mode::Insert;
                app.refresh_suggestions(store);
                Flow::Continue
            }
            _ => Flow::Continue,
        },
        Mode::Insert => match key.code {
            KeyCode::Esc => {
                app.mode = Mode::Normal;
                app.suggestions.clear();
                app.suggestion = 0;
                Flow::Continue
            }
            KeyCode::Enter => {
                app.execute(store);
                Flow::Continue
            }
            KeyCode::Backspace => {
                app.query.pop();
                app.refresh_suggestions(store);
                Flow::Continue
            }
            KeyCode::Char('u') if ctrl => {
                app.query.clear();
                app.refresh_suggestions(store);
                Flow::Continue
            }
            KeyCode::Char('c') if ctrl => Flow::Quit,
            KeyCode::Up => {
                app.suggestion_move(-1);
                Flow::Continue
            }
            KeyCode::Down => {
                app.suggestion_move(1);
                Flow::Continue
            }
            KeyCode::Tab => {
                app.accept_suggestion(store);
                Flow::Continue
            }
            KeyCode::Char(c) if !ctrl => {
                app.query.push(c);
                app.refresh_suggestions(store);
                Flow::Continue
            }
            _ => Flow::Continue,
        },
        Mode::Pick {
            input,
            candidates,
            selected,
        } => match key.code {
            KeyCode::Esc => {
                app.mode = Mode::Insert;
                app.refresh_suggestions(store);
                Flow::Continue
            }
            KeyCode::Char('q') if !ctrl => Flow::Quit,
            KeyCode::Char('c') if ctrl => Flow::Quit,
            KeyCode::Down | KeyCode::Char('j') => {
                app.pick_move(1);
                Flow::Continue
            }
            KeyCode::Up | KeyCode::Char('k') => {
                app.pick_move(-1);
                Flow::Continue
            }
            KeyCode::Enter => {
                let replacement = candidates
                    .get(selected)
                    .map(|_| format!("{}#{}", input, selected + 1));
                if let Some(replacement) = replacement {
                    app.query = replace_word(&app.query, &input, &replacement);
                }
                // Execute the fixed query; `execute` decides the next mode.
                app.mode = Mode::Insert;
                app.execute(store);
                Flow::Continue
            }
            _ => Flow::Continue,
        },
    }
}

fn draw(frame: &mut Frame<'_>, app: &App) {
    // The candidate chooser serves both tag suggestions (Insert) and the
    // ambiguity picker (Pick); it renders as a bordered list under the query.
    let chooser: Option<(&[Tag], usize, &str, bool)> = match &app.mode {
        Mode::Pick {
            input,
            candidates,
            selected,
        } => Some((candidates.as_slice(), *selected, input.as_str(), true)),
        Mode::Insert if !app.suggestions.is_empty() => {
            Some((app.suggestions.as_slice(), app.suggestion, "", false))
        }
        _ => None,
    };
    let chooser_height = match chooser {
        Some((tags, ..)) if !tags.is_empty() => (tags.len().min(CHOOSER_MAX_ROWS) + 2) as u16,
        _ => 0,
    };

    let [query_area, chooser_area, error_area, results_area, footer_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(chooser_height),
        Constraint::Length(app.error.is_some() as u16),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    frame.render_widget(
        Paragraph::new(format!("{QUERY_PREFIX}{}", app.query)),
        query_area,
    );

    if let Some((tags, selected, input, numbered)) = chooser {
        if !tags.is_empty() {
            let title = if numbered {
                format!(" ambiguous: {input} ")
            } else {
                " tags ".to_string()
            };
            let items: Vec<ListItem> = tags
                .iter()
                .enumerate()
                .map(|(i, tag)| {
                    let desc = if tag.description.is_empty() {
                        String::new()
                    } else {
                        format!(" — {}", tag.description)
                    };
                    let prefix = if numbered {
                        format!("#{}  ", i + 1)
                    } else {
                        String::new()
                    };
                    ListItem::new(Line::from(format!("{prefix}{}{desc}", tag.display_name())))
                })
                .collect();
            let list = List::new(items)
                .highlight_symbol("> ")
                .block(Block::bordered().title(title));
            let mut state = ListState::default().with_selected(Some(selected));
            frame.render_stateful_widget(list, chooser_area, &mut state);
        }
    }

    if let Some(error) = &app.error {
        frame.render_widget(
            Paragraph::new(error.as_str()).style(Style::default().fg(Color::Red)),
            error_area,
        );
    }

    if app.entries.is_empty() {
        frame.render_widget(Paragraph::new("No entries found."), results_area);
    } else {
        let items: Vec<ListItem> = app
            .entries
            .iter()
            .map(|entry| {
                ListItem::new(vec![
                    Line::from(entry.url.clone()),
                    Line::from(entry.description.clone()),
                    Line::from(app.entry_tags(entry)),
                    Line::default(),
                ])
            })
            .collect();
        let list = List::new(items).highlight_symbol("> ");
        let mut state = ListState::default().with_selected(Some(app.selected));
        frame.render_stateful_widget(list, results_area, &mut state);
    }

    let footer = match &app.mode {
        Mode::Normal => format!(
            "Page {}{}  |  Enter edit query  j/k move  n/p page  q/Esc quit",
            app.page + 1,
            if !app.entries.is_empty() && !app.has_more {
                " (last)"
            } else {
                ""
            }
        ),
        Mode::Insert => {
            "Enter run query (blank = all entries)  Tab complete  ↑/↓ choose  Esc back".to_string()
        }
        Mode::Pick { .. } => {
            "↑/↓ choose candidate  Enter use it  Esc edit query  q quit".to_string()
        }
    };
    frame.render_widget(Paragraph::new(footer), footer_area);

    if matches!(app.mode, Mode::Insert) {
        let mut x = query_area.x + (QUERY_PREFIX.len() + app.query.chars().count()) as u16;
        x = x.min(query_area.right().saturating_sub(1));
        frame.set_cursor_position(Position::new(x, query_area.y));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pantagraph::domain::{Entry, Tag};
    use std::path::PathBuf;

    fn temp_store(tag: &str) -> (JsonlStore, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "pantagraph_test_tui_{tag}_{}",
            uuid::Uuid::now_v7()
        ));
        let store = JsonlStore::new(&dir).unwrap();
        (store, dir)
    }

    /// Create a tag plus `count` entries tagged with it, URLs `https://example.test/{i}`.
    fn seed_entries(store: &JsonlStore, tag_name: &str, count: usize) -> TagId {
        let tag_id = store.create_tag(Tag::new(tag_name, "")).unwrap().id;
        for i in 0..count {
            store
                .create_entry(Entry::new(
                    format!("https://example.test/{i}"),
                    format!("entry {i}"),
                    vec![tag_id],
                ))
                .unwrap();
        }
        tag_id
    }

    fn urls(app: &App) -> Vec<String> {
        app.entries.iter().map(|e| e.url.clone()).collect()
    }

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn test_search_page_size_is_ten() {
        assert_eq!(entry::SEARCH_PAGE_SIZE, 10);
    }

    #[test]
    fn test_execute_limits_page_to_ten() {
        let (store, dir) = temp_store("page_size");
        seed_entries(&store, "alpha", 25);

        let mut app = App::new();
        app.query = "alpha".to_string();
        app.execute(&store);

        assert!(app.error.is_none());
        assert_eq!(app.entries.len(), entry::SEARCH_PAGE_SIZE);
        assert!(app.has_more);
        assert_eq!(app.entries[0].url, "https://example.test/0");
        assert_eq!(app.entries[9].url, "https://example.test/9");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_has_more_false_on_exact_page() {
        let (store, dir) = temp_store("exact_page");
        seed_entries(&store, "alpha", 10);

        let mut app = App::new();
        app.query = "alpha".to_string();
        app.execute(&store);

        assert_eq!(app.entries.len(), 10);
        assert!(!app.has_more);
        // The last page cannot be advanced past.
        app.next_page(&store);
        assert_eq!(app.page, 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_changing_query_resets_page() {
        let (store, dir) = temp_store("reset_page");
        seed_entries(&store, "alpha", 25);
        seed_entries(&store, "beta", 0);

        let mut app = App::new();
        app.query = "alpha".to_string();
        app.execute(&store);
        app.select_next();
        app.next_page(&store);
        assert_eq!(app.page, 1);
        assert_eq!(app.selected, 0);

        // Re-running the query returns to the first page and selection.
        app.select_next();
        assert_eq!(app.selected, 1);
        app.query = "alpha".to_string();
        app.execute(&store);
        assert_eq!(app.page, 0);
        assert_eq!(app.selected, 0);
        assert_eq!(app.entries[0].url, "https://example.test/0");

        // A new query with no results is also shown from page 1.
        app.next_page(&store);
        assert_eq!(app.page, 1);
        app.query = "alpha * beta".to_string();
        app.execute(&store);
        assert_eq!(app.page, 0);
        assert!(app.entries.is_empty());
        assert!(!app.has_more);
        assert!(app.error.is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_next_and_prev_page_change_offset() {
        let (store, dir) = temp_store("page_nav");
        seed_entries(&store, "alpha", 25);

        let mut app = App::new();
        app.query = "alpha".to_string();
        app.execute(&store);
        assert_eq!(app.page, 0);
        assert_eq!(app.entries[0].url, "https://example.test/0");

        app.next_page(&store);
        assert_eq!(app.page, 1);
        assert_eq!(app.entries.len(), 10);
        assert_eq!(app.entries[0].url, "https://example.test/10");

        app.next_page(&store);
        assert_eq!(app.page, 2);
        assert_eq!(app.entries.len(), 5);
        assert_eq!(app.entries[0].url, "https://example.test/20");
        assert!(!app.has_more);

        // Past the end is a no-op.
        app.next_page(&store);
        assert_eq!(app.page, 2);

        app.prev_page(&store);
        assert_eq!(app.page, 1);
        assert_eq!(app.entries[0].url, "https://example.test/10");

        app.prev_page(&store);
        assert_eq!(app.page, 0);
        assert_eq!(app.entries[0].url, "https://example.test/0");

        // Before the start is a no-op.
        app.prev_page(&store);
        assert_eq!(app.page, 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_query_errors_do_not_crash_and_keep_results() {
        let (store, dir) = temp_store("query_error");
        seed_entries(&store, "alpha", 3);

        let mut app = App::new();
        app.query = "alpha".to_string();
        app.execute(&store);
        assert!(app.error.is_none());
        let before = urls(&app);

        // Parse error is reported in the TUI; previous results survive.
        app.query = "alpha +".to_string();
        app.execute(&store);
        let error = app
            .error
            .as_deref()
            .expect("parse error should be reported");
        assert!(error.contains("invalid query"), "unexpected: {error}");
        assert_eq!(urls(&app), before);

        // Unknown tag is reported the same way.
        app.query = "no-such-tag".to_string();
        app.execute(&store);
        let error = app
            .error
            .as_deref()
            .expect("unknown tag should be reported");
        assert!(error.contains("Tag not found"), "unexpected: {error}");

        // A valid query clears the error.
        app.query = "alpha".to_string();
        app.execute(&store);
        assert!(app.error.is_none());
        assert_eq!(urls(&app), before);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_blank_query_lists_all_entries() {
        let (store, dir) = temp_store("blank_query");
        seed_entries(&store, "alpha", 3);

        let mut app = App::new();
        app.execute(&store);

        assert!(app.error.is_none());
        assert_eq!(app.entries.len(), 3);
        assert!(!app.has_more);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_selection_clamps_to_results() {
        let (store, dir) = temp_store("selection");
        seed_entries(&store, "alpha", 3);

        let mut app = App::new();
        app.query = "alpha".to_string();
        app.execute(&store);

        app.select_prev();
        assert_eq!(app.selected, 0);
        app.select_next();
        app.select_next();
        app.select_next();
        app.select_next();
        assert_eq!(app.selected, 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_word_at_end() {
        assert_eq!(word_at_end("rust"), "rust");
        assert_eq!(word_at_end("a + rus"), "rus");
        assert_eq!(word_at_end("rust * "), "");
        assert_eq!(word_at_end(""), "");
        assert_eq!(word_at_end("rust@lang#1"), "rust@lang#1");
        assert_eq!(word_at_end("x * rust@l"), "rust@l");
    }

    #[test]
    fn test_replace_word_boundaries() {
        assert_eq!(replace_word("rust", "rust", "rust#2"), "rust#2");
        assert_eq!(
            replace_word("a + rust * rust", "rust", "rust#1"),
            "a + rust#1 * rust#1"
        );
        // Substrings are left alone.
        assert_eq!(replace_word("rustacean", "rust", "rust#1"), "rustacean");
        // Delimiters and spacing are preserved.
        assert_eq!(
            replace_word("!rust (a + rust)", "rust", "rust#3"),
            "!rust#3 (a + rust#3)"
        );
        assert_eq!(replace_word("rust ", "rust", "rust#1"), "rust#1 ");
        // Empty word is a no-op.
        assert_eq!(replace_word("a b", "", "x"), "a b");
    }

    #[test]
    fn test_ambiguity_opens_picker_and_picks_candidate() {
        let (store, dir) = temp_store("picker");
        let first = store.create_tag(Tag::new("dup", "first")).unwrap().id;
        let second = store
            .create_tag(Tag::with_specifier(
                "dup",
                Some("light".to_string()),
                "second",
                vec![],
            ))
            .unwrap()
            .id;
        for (id, url) in [(first, "one"), (second, "two")] {
            store
                .create_entry(Entry::new(
                    format!("https://example.test/{url}"),
                    format!("entry {url}"),
                    vec![id],
                ))
                .unwrap();
        }

        let mut app = App::new();
        app.query = "dup".to_string();
        app.execute(&store);

        // The ambiguity is a selection list, not an error line.
        assert!(app.error.is_none());
        let Mode::Pick {
            input,
            candidates,
            selected,
        } = &app.mode
        else {
            panic!("ambiguous query should open the picker");
        };
        assert_eq!(input, "dup");
        assert_eq!(candidates.len(), 2);
        assert_eq!(*selected, 0);

        // Pick the second candidate; the query gains `#2` and runs.
        let flow = handle_key(&mut app, &store, press(KeyCode::Down));
        assert_eq!(flow, Flow::Continue);
        let flow = handle_key(&mut app, &store, press(KeyCode::Enter));
        assert_eq!(flow, Flow::Continue);
        assert!(matches!(app.mode, Mode::Normal));
        assert!(app.error.is_none());
        assert_eq!(app.query, "dup#2");
        assert_eq!(urls(&app), vec!["https://example.test/two".to_string()]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_picker_substitutes_every_occurrence() {
        let (store, dir) = temp_store("picker_multi");
        let first = store.create_tag(Tag::new("dup", "first")).unwrap().id;
        let second = store.create_tag(Tag::new("dup", "second")).unwrap().id;
        store
            .create_entry(Entry::new(
                "https://example.test/x".to_string(),
                "entry x".to_string(),
                vec![second],
            ))
            .unwrap();
        let _ = first;

        let mut app = App::new();
        app.query = "!dup * dup".to_string();
        app.execute(&store);
        assert!(matches!(app.mode, Mode::Pick { .. }));

        handle_key(&mut app, &store, press(KeyCode::Enter));
        assert_eq!(app.query, "!dup#1 * dup#1");
        assert!(app.error.is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_picker_escape_returns_to_insert() {
        let (store, dir) = temp_store("picker_esc");
        store.create_tag(Tag::new("dup", "one")).unwrap();
        store.create_tag(Tag::new("dup", "two")).unwrap();

        let mut app = App::new();
        app.query = "dup".to_string();
        app.execute(&store);
        assert!(matches!(app.mode, Mode::Pick { .. }));

        handle_key(&mut app, &store, press(KeyCode::Esc));
        assert!(matches!(app.mode, Mode::Insert));
        // Suggestions for the ambiguous word are shown again.
        assert_eq!(app.suggestions.len(), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_suggestions_offered_while_typing() {
        let (store, dir) = temp_store("suggestions");
        seed_entries(&store, "alpha", 1);
        seed_entries(&store, "alpine", 0);
        seed_entries(&store, "beta", 0);

        let mut app = App::new();
        app.query = "alp".to_string();
        app.mode = Mode::Insert;
        app.refresh_suggestions(&store);
        let names: Vec<String> = app.suggestions.iter().map(|t| t.name.clone()).collect();
        assert_eq!(names, vec!["alpha".to_string(), "alpine".to_string()]);

        // Arrow keys move the selection, clamped at both ends.
        assert_eq!(app.suggestion, 0);
        handle_key(&mut app, &store, press(KeyCode::Up));
        assert_eq!(app.suggestion, 0);
        handle_key(&mut app, &store, press(KeyCode::Down));
        assert_eq!(app.suggestion, 1);
        handle_key(&mut app, &store, press(KeyCode::Down));
        assert_eq!(app.suggestion, 1);

        // Tab completes the selected suggestion into the query.
        handle_key(&mut app, &store, press(KeyCode::Tab));
        assert_eq!(app.query, "alpine");
        assert!(matches!(app.mode, Mode::Insert));

        // The completed word has an exact-match suggestion again.
        assert!(!app.suggestions.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_suggestions_capped_at_limit() {
        let (store, dir) = temp_store("suggestions_cap");
        for i in 0..12 {
            store
                .create_tag(Tag::new(format!("sug-{i:02}"), ""))
                .unwrap();
        }

        let mut app = App::new();
        app.query = "sug-".to_string();
        app.refresh_suggestions(&store);
        assert_eq!(app.suggestions.len(), SUGGESTION_LIMIT);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_empty_word_has_no_suggestions() {
        let (store, dir) = temp_store("suggestions_empty");
        seed_entries(&store, "alpha", 1);

        let mut app = App::new();
        app.query = "alpha ".to_string();
        app.refresh_suggestions(&store);
        assert!(app.suggestions.is_empty());

        // Tab with nothing to complete is a no-op.
        handle_key(&mut app, &store, press(KeyCode::Tab));
        assert_eq!(app.query, "alpha ");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
