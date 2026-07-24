// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! A job's log — collapsible, coloured sections in a fixed-dark terminal.
//!
//! A relm4 child `Component` (root `adw::NavigationPage`), pushed when a job's
//! "View log" button is activated on the detail page. It fetches the whole job
//! log (`client::fetch_job_log`) and renders it the way GitHub's web UI does:
//!
//! - `##[group]…##[endgroup]` become **collapsible** `gtk::Expander` sections
//!   (a group containing an error auto-expands);
//! - the **ANSI colour** the tools emit (cargo/pnpm/eslint — yellow, green, cyan,
//!   bold, dim…) is parsed into `gtk::TextTag`s instead of being stripped;
//! - `##[command]` lines are shown as `$ …`, and `##[error]`/`##[warning]`/
//!   `##[notice]` annotations are coloured.
//!
//! Why per-*job*, not per-step: GitHub's raw log has no per-step delimiter (see
//! the history in `github::client`), so isolating a single step is guesswork.
//! The whole job, faithfully rendered, is both simpler and more useful.
//!
//! **Fetched, not streamed, completed jobs only** — a job still running shows a
//! "not available" state and points at the browser.
//!
//! The `.log-terminal` look is the second CLAUDE.md-sanctioned custom-CSS
//! exception (inherited from Dockyard): a console reads best on a stable dark
//! background, so this panel deliberately ignores the app's light/dark theme.

use std::collections::HashMap;

use octocrab::Octocrab;
use relm4::adw::prelude::*;
use relm4::{Component, ComponentParts, ComponentSender, adw, gtk, set_global_css};

use crate::github::client;

pub struct LogViewInit {
    pub octocrab: Octocrab,
    pub repo: String,
    pub job_id: u64,
    pub job_name: String,
    pub completed: bool,
    pub html_url: String,
}

pub struct LogView {
    job_name: String,
    html_url: String,
    state: LogState,
    /// The container the sections are built into once the log arrives — held so
    /// the load handler can fill it imperatively (the widgets are dynamic).
    body: gtk::Box,
}

#[derive(Debug)]
enum LogState {
    /// The job hasn't finished — no log to download yet.
    NotAvailable,
    Loading,
    Loaded,
    Error(String),
}

#[derive(Debug)]
pub enum LogViewOutput {
    OpenInBrowser(String),
}

#[derive(Debug)]
pub enum LogViewCmd {
    /// The parsed log (off-thread), or a fetch error.
    Loaded(Box<Result<Vec<Block>, String>>),
}

impl LogView {
    fn page(&self) -> &'static str {
        match self.state {
            LogState::NotAvailable => "not-available",
            LogState::Loading => "loading",
            LogState::Loaded => "log",
            LogState::Error(_) => "error",
        }
    }

    fn error(&self) -> String {
        match &self.state {
            LogState::Error(message) => message.clone(),
            _ => String::new(),
        }
    }

    /// Build one section widget per parsed block into `body`. Called once, on
    /// load, so there's nothing to clear first.
    fn build_sections(&self, blocks: Vec<Block>) {
        for block in blocks {
            match block {
                Block::Loose(lines) => {
                    self.body.append(&build_text_view(&lines, false));
                }
                Block::Group {
                    title,
                    lines,
                    has_error,
                } => {
                    let expander = gtk::Expander::new(None);
                    expander.add_css_class("log-terminal");
                    let label = gtk::Label::new(Some(&title));
                    label.set_xalign(0.0);
                    label.add_css_class("log-terminal");
                    label.add_css_class("log-group");
                    expander.set_label_widget(Some(&label));
                    // Collapsed by default (like GitHub), but a failing group
                    // opens itself so the error is visible without a click.
                    expander.set_expanded(has_error);
                    expander.set_child(Some(&build_text_view(&lines, true)));
                    self.body.append(&expander);
                }
            }
        }
    }
}

/// Install the terminal stylesheet. Called from `main` once GTK is up.
pub fn install_css() {
    set_global_css(CSS);
}

// Fixed dark, theme-independent. The colour is pinned on the `text` node (the
// TextView's text area), because the theme sets that node's colour explicitly
// and a merely-inherited value would lose to it. `.log-group` gives the group
// headers a touch of weight so the collapsible sections read as headers.
const CSS: &str = "
.log-terminal { background-color: #1d1f21; color: #c5c8c6; }
.log-terminal text { background-color: #1d1f21; color: #c5c8c6; }
.log-group { font-weight: bold; padding: 2px 0; }
";

#[relm4::component(pub)]
impl Component for LogView {
    type Init = LogViewInit;
    type Input = ();
    type Output = LogViewOutput;
    type CommandOutput = LogViewCmd;

    view! {
        adw::NavigationPage {
            set_title: &model.job_name,

            adw::ToolbarView {
                add_top_bar = &adw::HeaderBar {
                    pack_end = &gtk::Button {
                        set_icon_name: "web-browser-symbolic",
                        set_tooltip_text: Some("Open in browser"),
                        connect_clicked[sender, url = model.html_url.clone()] => move |_| {
                            sender.output(LogViewOutput::OpenInBrowser(url.clone())).ok();
                        },
                    },
                },

                #[wrap(Some)]
                set_content = &gtk::Stack {
                    add_named[Some("not-available")] = &adw::StatusPage {
                        set_icon_name: Some("content-loading-symbolic"),
                        set_title: "Logs not available yet",
                        set_description: Some(
                            "This job hasn't finished. Open the run in your browser to \
                             follow it live.",
                        ),
                    },

                    add_named[Some("loading")] = &gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                        set_valign: gtk::Align::Center,
                        set_halign: gtk::Align::Center,
                        gtk::Spinner {
                            set_spinning: true,
                            set_size_request: (32, 32),
                        },
                    },

                    add_named[Some("error")] = &adw::StatusPage {
                        set_icon_name: Some("network-error-symbolic"),
                        set_title: "Couldn't load the log",
                        #[watch]
                        set_description: Some(&model.error()),
                    },

                    add_named[Some("log")] = &gtk::ScrolledWindow {
                        set_vexpand: true,
                        add_css_class: "log-terminal",

                        #[local_ref]
                        body -> gtk::Box {
                            set_orientation: gtk::Orientation::Vertical,
                            set_vexpand: true,
                            add_css_class: "log-terminal",
                        },
                    },

                    #[watch]
                    set_visible_child_name: model.page(),
                },
            },
        }
    }

    fn init(
        init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let body = gtk::Box::new(gtk::Orientation::Vertical, 0);

        let model = LogView {
            job_name: init.job_name,
            html_url: init.html_url,
            state: if init.completed {
                LogState::Loading
            } else {
                LogState::NotAvailable
            },
            body: body.clone(),
        };

        let widgets = view_output!();

        if init.completed {
            let octocrab = init.octocrab;
            let repo = init.repo;
            let job_id = init.job_id;
            // Fetch *and* parse off-thread: parsing a ~150 KB log into coloured
            // spans shouldn't touch the GTK main thread. Widgets are built from
            // the parsed blocks in `update_cmd`, which runs back on it.
            sender.oneshot_command(async move {
                let outcome = match repo.split_once('/') {
                    Some((owner, name)) => client::fetch_job_log(&octocrab, owner, name, job_id)
                        .await
                        .map(|raw| parse_job_log(&raw)),
                    None => Err(format!("“{repo}” isn't a valid owner/name")),
                };
                LogViewCmd::Loaded(Box::new(outcome))
            });
        }

        ComponentParts { model, widgets }
    }

    fn update_cmd(
        &mut self,
        msg: Self::CommandOutput,
        _sender: ComponentSender<Self>,
        _root: &Self::Root,
    ) {
        match msg {
            LogViewCmd::Loaded(result) => match *result {
                Ok(blocks) => {
                    self.build_sections(blocks);
                    self.state = LogState::Loaded;
                }
                Err(message) => {
                    self.state = LogState::Error(message);
                }
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Parsing: raw job log -> collapsible, coloured blocks. Pure functions (no GTK),
// so they run off-thread and are unit-tested below.
// ---------------------------------------------------------------------------

/// A run of loose lines, or one collapsible group.
#[derive(Debug)]
pub enum Block {
    Loose(Vec<Line>),
    Group {
        title: String,
        lines: Vec<Line>,
        /// Whether the group contains an `##[error]` — it auto-expands if so.
        has_error: bool,
    },
}

/// One log line: its role (for colouring) and its coloured spans.
#[derive(Debug)]
pub struct Line {
    kind: LineKind,
    spans: Vec<Span>,
}

/// A run of text sharing one style, carved out of a line by its ANSI codes.
#[derive(Debug)]
pub struct Span {
    text: String,
    /// Foreground colour from ANSI (a palette hex), or `None` for the default.
    color: Option<&'static str>,
    bold: bool,
    underline: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineKind {
    Normal,
    Command,
    Error,
    Warning,
    Notice,
}

// Tomorrow-Night palette, tuned for the #1d1f21 terminal (foreground only —
// backgrounds are rare in these logs and messy in a text view, so ignored).
const FG_RED: &str = "#cc6666";
const FG_GREEN: &str = "#b5bd68";
const FG_YELLOW: &str = "#f0c674";
const FG_BLUE: &str = "#81a2be";
const FG_MAGENTA: &str = "#b294bb";
const FG_CYAN: &str = "#8abeb7";
const FG_WHITE: &str = "#ffffff";
const FG_GREY: &str = "#969896";
const FG_DIM: &str = "#808486";

/// Map an ANSI SGR foreground code (30–37, 90–97) to a palette colour.
fn ansi_color(code: u16) -> Option<&'static str> {
    Some(match code {
        // True black would vanish on a dark terminal, so map it to grey.
        30 | 90 => FG_GREY,
        31 | 91 => FG_RED,
        32 | 92 => FG_GREEN,
        33 | 93 => FG_YELLOW,
        34 | 94 => FG_BLUE,
        35 | 95 => FG_MAGENTA,
        36 | 96 => FG_CYAN,
        37 | 97 => FG_WHITE,
        _ => return None,
    })
}

/// The colour a whole line's kind implies, used for spans that carry no ANSI
/// colour of their own.
fn line_color(kind: LineKind) -> Option<&'static str> {
    match kind {
        LineKind::Command => Some(FG_CYAN),
        LineKind::Error => Some(FG_RED),
        LineKind::Warning => Some(FG_YELLOW),
        LineKind::Notice => Some(FG_BLUE),
        LineKind::Normal => None,
    }
}

/// Parse a whole raw job log into blocks. Groups nest occasionally; we flatten a
/// nested `##[group]` into a new top-level section (simpler than nested
/// expanders, and still readable).
fn parse_job_log(raw: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut loose: Vec<Line> = Vec::new();
    // (title, lines, has_error) for the group currently open, if any.
    let mut group: Option<(String, Vec<Line>, bool)> = None;

    let close_group = |group: &mut Option<(String, Vec<Line>, bool)>, blocks: &mut Vec<Block>| {
        if let Some((title, lines, has_error)) = group.take() {
            blocks.push(Block::Group {
                title,
                lines,
                has_error,
            });
        }
    };

    for raw_line in raw.lines() {
        let content = strip_timestamp(raw_line);
        let probe = content.trim_start();

        if let Some(rest) = probe.strip_prefix("##[group]") {
            if !loose.is_empty() {
                blocks.push(Block::Loose(std::mem::take(&mut loose)));
            }
            close_group(&mut group, &mut blocks);
            group = Some((
                spans_text(&parse_ansi(rest)).trim().to_owned(),
                Vec::new(),
                false,
            ));
            continue;
        }
        if probe == "##[endgroup]" {
            close_group(&mut group, &mut blocks);
            continue;
        }

        let (kind, text) = classify(probe, content);
        let line = Line {
            kind,
            spans: parse_ansi(text),
        };
        match &mut group {
            Some((_, lines, has_error)) => {
                *has_error |= kind == LineKind::Error;
                lines.push(line);
            }
            None => loose.push(line),
        }
    }

    close_group(&mut group, &mut blocks);
    if !loose.is_empty() {
        blocks.push(Block::Loose(loose));
    }
    blocks
}

/// Classify a line by its `##[…]` annotation. Non-annotated lines keep their
/// original indentation (`content`); annotated ones return the text after the
/// marker.
fn classify<'a>(probe: &'a str, content: &'a str) -> (LineKind, &'a str) {
    if let Some(rest) = probe.strip_prefix("##[command]") {
        (LineKind::Command, rest)
    } else if let Some(rest) = probe.strip_prefix("##[error]") {
        (LineKind::Error, rest)
    } else if let Some(rest) = probe.strip_prefix("##[warning]") {
        (LineKind::Warning, rest)
    } else if let Some(rest) = probe.strip_prefix("##[notice]") {
        (LineKind::Notice, rest)
    } else if let Some(rest) = probe.strip_prefix("##[section]") {
        (LineKind::Normal, rest)
    } else {
        (LineKind::Normal, content)
    }
}

/// Split text into styled spans by interpreting its ANSI SGR (`ESC[…m`) escapes.
/// Non-SGR CSI sequences (cursor moves and the like) are dropped.
fn parse_ansi(text: &str) -> Vec<Span> {
    let mut spans = Vec::new();
    let mut current = String::new();
    let (mut color, mut bold, mut dim, mut underline) = (None, false, false, false);

    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            if !current.is_empty() {
                spans.push(Span {
                    text: std::mem::take(&mut current),
                    color: color.or(if dim { Some(FG_DIM) } else { None }),
                    bold,
                    underline,
                });
            }
            chars.next(); // consume '['
            let mut params = String::new();
            let mut final_byte = '\0';
            for nc in chars.by_ref() {
                if nc.is_ascii_alphabetic() {
                    final_byte = nc;
                    break;
                }
                params.push(nc);
            }
            if final_byte == 'm' {
                apply_sgr(&params, &mut color, &mut bold, &mut dim, &mut underline);
            }
        } else {
            current.push(c);
        }
    }
    if !current.is_empty() {
        spans.push(Span {
            text: current,
            color: color.or(if dim { Some(FG_DIM) } else { None }),
            bold,
            underline,
        });
    }
    spans
}

/// Apply one `ESC[…m` parameter list to the running style. Empty params = reset.
fn apply_sgr(
    params: &str,
    color: &mut Option<&'static str>,
    bold: &mut bool,
    dim: &mut bool,
    underline: &mut bool,
) {
    let codes: Vec<u16> = if params.is_empty() {
        vec![0]
    } else {
        params.split(';').filter_map(|p| p.parse().ok()).collect()
    };
    let mut i = 0;
    while i < codes.len() {
        match codes[i] {
            0 => {
                *color = None;
                *bold = false;
                *dim = false;
                *underline = false;
            }
            1 => *bold = true,
            2 => *dim = true,
            4 => *underline = true,
            22 => {
                *bold = false;
                *dim = false;
            }
            24 => *underline = false,
            39 => *color = None,
            30..=37 | 90..=97 => *color = ansi_color(codes[i]),
            // Extended colour: 38;5;n or 38;2;r;g;b — skip its arguments rather
            // than misread them as separate codes.
            38 => match codes.get(i + 1) {
                Some(5) => i += 2,
                Some(2) => i += 4,
                _ => {}
            },
            _ => {} // backgrounds (40–49) and anything else: ignored
        }
        i += 1;
    }
}

/// Concatenate spans back to plain text — for group titles, which we show
/// un-styled.
fn spans_text(spans: &[Span]) -> String {
    spans.iter().map(|s| s.text.as_str()).collect()
}

/// Drop a `2026-07-24T14:30:01.12Z ` timestamp prefix (and a leading UTF-8 BOM on
/// the very first line), if present.
fn strip_timestamp(line: &str) -> &str {
    let line = line.strip_prefix('\u{feff}').unwrap_or(line);
    if let Some((first, rest)) = line.split_once(' ')
        && first.len() >= 20
        && first.ends_with('Z')
        && first.as_bytes().get(4) == Some(&b'-')
    {
        return rest;
    }
    line
}

// ---------------------------------------------------------------------------
// Rendering: parsed blocks -> GTK widgets (main thread).
// ---------------------------------------------------------------------------

/// Build a monospace, fixed-dark `TextView` for a block's lines, applying the
/// span colours as `TextTag`s. `indented` insets a group's body under its header.
fn build_text_view(lines: &[Line], indented: bool) -> gtk::TextView {
    let view = gtk::TextView::new();
    view.set_editable(false);
    view.set_cursor_visible(false);
    view.set_monospace(true);
    view.set_wrap_mode(gtk::WrapMode::WordChar);
    view.set_left_margin(if indented { 24 } else { 8 });
    view.set_right_margin(8);
    view.set_top_margin(4);
    view.set_bottom_margin(4);
    view.add_css_class("log-terminal");

    let buffer = view.buffer();
    // Reuse one TextTag per distinct style — a log has only a handful.
    let mut tags: HashMap<(Option<&'static str>, bool, bool), gtk::TextTag> = HashMap::new();

    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            let mut iter = buffer.end_iter();
            buffer.insert(&mut iter, "\n");
        }
        if line.kind == LineKind::Command {
            let tag = tag_for(&buffer, &mut tags, Some(FG_CYAN), false, false);
            let mut iter = buffer.end_iter();
            buffer.insert_with_tags(&mut iter, "$ ", &[&tag]);
        }
        for span in &line.spans {
            let color = span.color.or(line_color(line.kind));
            let tag = tag_for(&buffer, &mut tags, color, span.bold, span.underline);
            let mut iter = buffer.end_iter();
            buffer.insert_with_tags(&mut iter, &span.text, &[&tag]);
        }
    }
    view
}

/// Get (or create and memoise) a `TextTag` for a style on this buffer.
fn tag_for(
    buffer: &gtk::TextBuffer,
    tags: &mut HashMap<(Option<&'static str>, bool, bool), gtk::TextTag>,
    color: Option<&'static str>,
    bold: bool,
    underline: bool,
) -> gtk::TextTag {
    tags.entry((color, bold, underline))
        .or_insert_with(|| {
            let tag = gtk::TextTag::new(None);
            if let Some(color) = color {
                tag.set_foreground(Some(color));
            }
            if bold {
                tag.set_weight(700);
            }
            if underline {
                tag.set_underline(gtk::pango::Underline::Single);
            }
            buffer.tag_table().add(&tag);
            tag
        })
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(line: &str) -> String {
        format!("2026-07-21T18:23:00.1234567Z {line}")
    }

    #[test]
    fn strips_timestamp_and_bom() {
        assert_eq!(strip_timestamp("2026-07-21T18:23:00.1Z hello"), "hello");
        assert_eq!(
            strip_timestamp("\u{feff}2026-07-21T18:23:00.1Z first"),
            "first"
        );
        // No timestamp: returned unchanged.
        assert_eq!(strip_timestamp("  indented output"), "  indented output");
    }

    #[test]
    fn parses_ansi_into_styled_spans() {
        let spans = parse_ansi("\u{1b}[32mok\u{1b}[0m done");
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].text, "ok");
        assert_eq!(spans[0].color, Some(FG_GREEN));
        assert_eq!(spans[1].text, " done");
        assert_eq!(spans[1].color, None);
    }

    #[test]
    fn bold_dim_and_reset_track_the_running_style() {
        // `22` (normal intensity) turns bold/dim off but keeps the colour — it's
        // not a colour reset; only `0` resets everything.
        let spans = parse_ansi("\u{1b}[1;33mbold\u{1b}[22m still-yellow\u{1b}[0mplain");
        assert_eq!(spans[0].text, "bold");
        assert!(spans[0].bold);
        assert_eq!(spans[0].color, Some(FG_YELLOW));
        assert_eq!(spans[1].text, " still-yellow");
        assert!(!spans[1].bold);
        assert_eq!(spans[1].color, Some(FG_YELLOW));
        assert_eq!(spans[2].text, "plain");
        assert_eq!(spans[2].color, None);

        // Dim with no explicit colour renders as the dim grey.
        let dimmed = parse_ansi("\u{1b}[2mfaint");
        assert_eq!(dimmed[0].text, "faint");
        assert_eq!(dimmed[0].color, Some(FG_DIM));
    }

    #[test]
    fn groups_close_and_flag_errors() {
        let raw = [
            ts("preamble line"),
            ts("##[group]Run pnpm test"),
            ts("running tests"),
            ts("##[error]a test failed"),
            ts("##[endgroup]"),
            ts("trailing line"),
        ]
        .join("\n");

        let blocks = parse_job_log(&raw);
        assert_eq!(blocks.len(), 3);

        match &blocks[0] {
            Block::Loose(lines) => assert_eq!(lines[0].spans[0].text, "preamble line"),
            other => panic!("expected loose preamble, got {other:?}"),
        }
        match &blocks[1] {
            Block::Group {
                title,
                lines,
                has_error,
            } => {
                assert_eq!(title, "Run pnpm test");
                assert_eq!(lines.len(), 2);
                assert!(*has_error);
            }
            other => panic!("expected group, got {other:?}"),
        }
        match &blocks[2] {
            Block::Loose(lines) => assert_eq!(lines[0].spans[0].text, "trailing line"),
            other => panic!("expected trailing loose, got {other:?}"),
        }
    }

    #[test]
    fn nested_group_flattens_into_a_new_section() {
        let raw = [
            ts("##[group]Outer"),
            ts("outer line"),
            ts("##[group]Inner"),
            ts("inner line"),
            ts("##[endgroup]"),
        ]
        .join("\n");

        let blocks = parse_job_log(&raw);
        // Outer is closed when Inner opens, so two top-level groups result.
        assert_eq!(blocks.len(), 2);
        assert!(matches!(&blocks[0], Block::Group { title, .. } if title == "Outer"));
        assert!(matches!(&blocks[1], Block::Group { title, .. } if title == "Inner"));
    }
}
