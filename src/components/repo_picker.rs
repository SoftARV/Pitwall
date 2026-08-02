// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! The repo picker — choose which repositories to watch.
//!
//! A relm4 child `Component` whose root is an `adw::Dialog`. It fetches the
//! repos the token can see (off-thread), lists them as toggle rows with a live
//! search, and outputs the chosen set for the app to persist. octocrab is used
//! only to fetch; the UI sees our `Repo` type (CLAUDE.md rule 4).

use std::collections::HashSet;

use octocrab::Octocrab;
use relm4::adw::prelude::*;
use relm4::{Component, ComponentParts, ComponentSender, RelmWidgetExt, adw, gtk};

use crate::github::client;
use crate::github::types::Repo;

pub struct RepoPicker {
    /// The full names (`owner/name`) currently ticked. Seeded from the current
    /// watch set, mutated as switches toggle, emitted on Save.
    selected: HashSet<String>,
    state: PickerState,
    /// Held so the load handler can populate it and search can re-filter it —
    /// the same imperative-handle reason the app holds its `nav`/`toast_overlay`.
    /// (The search entry needs no handle: its `filter_func` and `search-changed`
    /// closures capture their own clones.)
    list_box: gtk::ListBox,
}

#[derive(Debug)]
enum PickerState {
    Loading,
    Loaded,
    Error(String),
}

pub struct RepoPickerInit {
    pub octocrab: Octocrab,
    pub watched: Vec<String>,
}

#[derive(Debug)]
pub enum RepoPickerInput {
    /// A repo's switch flipped: (full_name, now-on).
    Toggle(String, bool),
    /// Save the current selection and close.
    Save,
}

#[derive(Debug)]
pub enum RepoPickerOutput {
    /// The user saved a watch set (sorted full names).
    Saved(Vec<String>),
}

#[derive(Debug)]
pub enum RepoPickerCmd {
    /// The repo list came back (or an error message).
    Loaded(Box<Result<Vec<Repo>, String>>),
}

impl RepoPicker {
    fn page(&self) -> &'static str {
        match self.state {
            PickerState::Loading => "loading",
            PickerState::Loaded => "list",
            PickerState::Error(_) => "error",
        }
    }

    fn error(&self) -> String {
        match &self.state {
            PickerState::Error(message) => message.clone(),
            _ => String::new(),
        }
    }

    /// (Re)build the switch rows from a fetched repo list. Each row is an
    /// `adw::SwitchRow`; toggling it posts a `Toggle` so the model stays the
    /// single source of truth for the selection.
    fn populate(&self, repos: &[Repo], sender: &ComponentSender<Self>) {
        while let Some(child) = self.list_box.first_child() {
            self.list_box.remove(&child);
        }

        let add_row = |full_name: String, subtitle: &str, orphan: bool| {
            let row = adw::SwitchRow::new();
            row.set_title(&full_name);
            row.set_subtitle(subtitle);
            row.set_active(self.selected.contains(&full_name));
            if orphan {
                row.add_css_class("warning");
            }

            let input = sender.input_sender().clone();
            row.connect_active_notify(move |row| {
                input
                    .send(RepoPickerInput::Toggle(full_name.clone(), row.is_active()))
                    .ok();
            });
            self.list_box.append(&row);
        };

        // Anything watched that GitHub didn't list back — renamed, deleted, or no
        // longer accessible. Without a row there'd be no way to untick it, and it
        // would be re-saved (still selected) every time. Listed first, since it's
        // the thing most likely to need attention.
        let listed: HashSet<String> = repos.iter().map(Repo::full_name).collect();
        let mut orphans: Vec<&String> = self
            .selected
            .iter()
            .filter(|name| !listed.contains(*name))
            .collect();
        orphans.sort();
        for name in orphans {
            add_row(
                name.clone(),
                "Not found — renamed, deleted, or no longer accessible",
                true,
            );
        }

        for repo in repos {
            add_row(
                repo.full_name(),
                if repo.private { "Private" } else { "Public" },
                false,
            );
        }
    }
}

#[relm4::component(pub)]
impl Component for RepoPicker {
    type Init = RepoPickerInit;
    type Input = RepoPickerInput;
    type Output = RepoPickerOutput;
    type CommandOutput = RepoPickerCmd;

    view! {
        adw::Dialog {
            set_title: "Watch Repositories",
            set_content_width: 460,
            set_content_height: 640,

            #[wrap(Some)]
            set_child = &adw::ToolbarView {
                add_top_bar = &adw::HeaderBar {
                    pack_end = &gtk::Button {
                        set_label: "Save",
                        add_css_class: "suggested-action",
                        connect_clicked => RepoPickerInput::Save,
                    },
                },

                #[wrap(Some)]
                set_content = &gtk::Stack {
                    add_named[Some("loading")] = &gtk::Box {
                        set_valign: gtk::Align::Center,
                        set_halign: gtk::Align::Center,
                        gtk::Spinner {
                            set_spinning: true,
                            set_size_request: (32, 32),
                        },
                    },

                    add_named[Some("error")] = &adw::StatusPage {
                        set_icon_name: Some("network-error-symbolic"),
                        set_title: "Couldn't load repositories",
                        #[watch]
                        set_description: Some(&model.error()),
                    },

                    add_named[Some("list")] = &gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,

                        #[local_ref]
                        search -> gtk::SearchEntry {
                            set_margin_all: 12,
                            set_placeholder_text: Some("Search repositories"),
                        },

                        gtk::ScrolledWindow {
                            set_vexpand: true,

                            adw::Clamp {
                                set_maximum_size: 500,
                                set_margin_all: 12,

                                #[local_ref]
                                list_box -> gtk::ListBox {
                                    set_valign: gtk::Align::Start,
                                    set_selection_mode: gtk::SelectionMode::None,
                                    add_css_class: "boxed-list",
                                },
                            },
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
        let list_box = gtk::ListBox::new();
        let search = gtk::SearchEntry::new();

        // Filter the rows by the search text GTK-side — a `filter_func` plus an
        // `invalidate_filter` on each keystroke, so there's no row rebuild and no
        // "current query" field to keep in sync (the entry is the source of truth).
        let filter_search = search.clone();
        list_box.set_filter_func(move |row| {
            let query = filter_search.text().to_lowercase();
            query.is_empty()
                || row
                    .downcast_ref::<adw::SwitchRow>()
                    .map(|switch| switch.title().to_lowercase().contains(&query))
                    .unwrap_or(true)
        });
        let refilter = list_box.clone();
        search.connect_search_changed(move |_| refilter.invalidate_filter());

        let model = RepoPicker {
            selected: init.watched.into_iter().collect(),
            state: PickerState::Loading,
            list_box: list_box.clone(),
        };

        let widgets = view_output!();

        // Fetch the watchable repos off-thread (CLAUDE.md rule 5).
        let octocrab = init.octocrab;
        sender.oneshot_command(async move {
            RepoPickerCmd::Loaded(Box::new(
                client::list_repos(&octocrab)
                    .await
                    .map_err(|err| err.message().to_owned()),
            ))
        });

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>, root: &Self::Root) {
        match msg {
            RepoPickerInput::Toggle(full_name, on) => {
                if on {
                    self.selected.insert(full_name);
                } else {
                    self.selected.remove(&full_name);
                }
            }
            RepoPickerInput::Save => {
                let mut names: Vec<String> = self.selected.iter().cloned().collect();
                names.sort();
                sender.output(RepoPickerOutput::Saved(names)).ok();
                root.close();
            }
        }
    }

    fn update_cmd(
        &mut self,
        msg: Self::CommandOutput,
        sender: ComponentSender<Self>,
        _root: &Self::Root,
    ) {
        match msg {
            RepoPickerCmd::Loaded(result) => match *result {
                Ok(repos) => {
                    self.populate(&repos, &sender);
                    self.state = PickerState::Loaded;
                }
                Err(message) => {
                    self.state = PickerState::Error(message);
                }
            },
        }
    }
}
