// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! One step's log — a fixed-dark terminal panel.
//!
//! A relm4 child `Component` (root `adw::NavigationPage`), pushed when a step is
//! activated on the detail page. It fetches the step's log (via the run's log
//! zip, `client::fetch_step_log`) and shows the cleaned text in a monospace,
//! fixed-dark view. **Fetched, not streamed, completed steps only** — a step
//! that hasn't finished shows a "not available" state and points at the browser.
//!
//! The `.log-terminal` look is the second CLAUDE.md-sanctioned custom-CSS
//! exception (inherited from Dockyard): a console reads best on a stable dark
//! background, so this one panel deliberately ignores the app's light/dark theme.

use octocrab::Octocrab;
use relm4::adw::prelude::*;
use relm4::{Component, ComponentParts, ComponentSender, adw, gtk, set_global_css};

use crate::github::client;

pub struct LogViewInit {
    pub octocrab: Octocrab,
    pub repo: String,
    pub run_id: u64,
    pub job_name: String,
    pub step_number: i64,
    pub step_name: String,
    pub completed: bool,
    pub html_url: String,
}

pub struct LogView {
    step_name: String,
    html_url: String,
    state: LogState,
    /// The terminal's text buffer, filled once the log arrives.
    buffer: gtk::TextBuffer,
}

#[derive(Debug)]
enum LogState {
    /// The step hasn't finished — no log to download yet.
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
    Loaded(Box<Result<String, String>>),
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
}

/// Install the terminal stylesheet. Called from `main` once GTK is up.
pub fn install_css() {
    set_global_css(CSS);
}

// Fixed dark, theme-independent. The colour is pinned on the `text` node (the
// TextView's text area), because the theme sets that node's colour explicitly
// and a merely-inherited value would lose to it.
const CSS: &str = "
.log-terminal { background-color: #1d1f21; color: #c5c8c6; }
.log-terminal text { background-color: #1d1f21; color: #c5c8c6; }
";

#[relm4::component(pub)]
impl Component for LogView {
    type Init = LogViewInit;
    type Input = ();
    type Output = LogViewOutput;
    type CommandOutput = LogViewCmd;

    view! {
        adw::NavigationPage {
            set_title: &model.step_name,

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
                            "This step hasn't finished. Open the run in your browser to \
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

                        #[name = "log_text"]
                        gtk::TextView {
                            set_editable: false,
                            set_cursor_visible: false,
                            set_monospace: true,
                            set_top_margin: 8,
                            set_bottom_margin: 8,
                            set_left_margin: 8,
                            set_right_margin: 8,
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
        let buffer = gtk::TextBuffer::new(None);

        let model = LogView {
            step_name: init.step_name.clone(),
            html_url: init.html_url,
            state: if init.completed {
                LogState::Loading
            } else {
                LogState::NotAvailable
            },
            buffer: buffer.clone(),
        };

        let widgets = view_output!();
        widgets.log_text.set_buffer(Some(&buffer));

        if init.completed {
            let octocrab = init.octocrab;
            let repo = init.repo;
            let job_name = init.job_name;
            let step_name = init.step_name;
            let (run_id, step_number) = (init.run_id, init.step_number);
            sender.oneshot_command(async move {
                let outcome = match repo.split_once('/') {
                    Some((owner, name)) => {
                        client::fetch_step_log(
                            &octocrab,
                            owner,
                            name,
                            run_id,
                            &job_name,
                            step_number,
                            &step_name,
                        )
                        .await
                    }
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
                Ok(text) => {
                    self.buffer.set_text(&text);
                    self.state = LogState::Loaded;
                }
                Err(message) => {
                    self.state = LogState::Error(message);
                }
            },
        }
    }
}
