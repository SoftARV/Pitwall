// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! One workflow run, rendered as an `adw::ActionRow`.
//!
//! The row emits an intent (open the run on github.com); `AppModel::update` owns
//! what happens, keeping all decisions in the one reducer. Poll updates arrive as
//! `Update`, applied in place via `#[watch]` — no rebuild (CLAUDE.md rule 7).

use chrono::{DateTime, Utc};
use relm4::adw::prelude::*;
use relm4::factory::{DynamicIndex, FactoryComponent, FactorySender};
use relm4::{adw, gtk};

use crate::components::status_chip::{self, StatusChip};
use crate::github::types::{RunStatus, WorkflowRun};

#[derive(Debug)]
pub enum RunRowOutput {
    /// Open this run's native detail page (the row was activated).
    ShowDetails(u64),
    /// Open this run's page on github.com (the browser button).
    OpenInBrowser(String),
}

#[derive(Debug)]
pub enum RunRowInput {
    /// Fresh data for this run from the poll, applied in place.
    Update(WorkflowRun),
}

pub struct RunRow {
    run: WorkflowRun,
}

impl RunRow {
    /// Lets the parent reconcile rows against incoming runs by id, without cloning.
    pub fn id(&self) -> u64 {
        self.run.id
    }

    /// "CI #128"
    fn title(&self) -> String {
        format!("{} #{}", self.run.workflow_name, self.run.run_number)
    }

    /// "SoftARV/Pitwall · main · push · 5m ago"
    fn subtitle(&self) -> String {
        format!(
            "{} · {} · {} · {}",
            self.run.repo,
            self.run.head_branch,
            self.run.event,
            relative(self.run.created_at),
        )
    }
}

/// The primary run-action button's look and target action, by run state: cancel
/// while it's running, re-run once it's finished. Shared by the list row and the
/// detail page so both read identically. Each button binds directly to the shared
/// "runs" action (registered on the window in `app`) with the run id as its
/// target — one click, no menu. (Re-run-*failed* is a detail-page extra.)
pub(crate) struct RunActionSpec {
    pub icon: &'static str,
    pub label: &'static str,
    pub action: &'static str,
    /// False while the run is already cancelling — the button shows the state but
    /// can't be re-clicked.
    pub enabled: bool,
}

pub(crate) fn run_action_spec(status: RunStatus) -> RunActionSpec {
    match status {
        // Already cancelling: show it, but disabled — there's nothing more to do.
        RunStatus::Cancelling => RunActionSpec {
            icon: "process-stop-symbolic",
            label: "Cancelling…",
            action: "runs.cancel",
            enabled: false,
        },
        status if status.is_active() => RunActionSpec {
            icon: "process-stop-symbolic",
            label: "Cancel",
            action: "runs.cancel",
            enabled: true,
        },
        _ => RunActionSpec {
            icon: "view-refresh-symbolic",
            label: "Re-run",
            action: "runs.rerun",
            enabled: true,
        },
    }
}

/// A compact "5m ago" for the run's start time. Recomputed on each `Update`, so
/// it stays roughly current as the poll refreshes rows. A clock skew that puts
/// the run in the future reads as "just now" rather than a negative age. Shared
/// with the repos view.
pub(crate) fn relative(when: DateTime<Utc>) -> String {
    let seconds = Utc::now().signed_duration_since(when).num_seconds();
    match seconds {
        s if s < 45 => "just now".to_owned(),
        s if s < 90 => "1m ago".to_owned(),
        s if s < 3600 => format!("{}m ago", s / 60),
        s if s < 7200 => "1h ago".to_owned(),
        s if s < 86_400 => format!("{}h ago", s / 3600),
        s if s < 172_800 => "1d ago".to_owned(),
        s => format!("{}d ago", s / 86_400),
    }
}

#[relm4::factory(pub)]
impl FactoryComponent for RunRow {
    type Init = WorkflowRun;
    type Input = RunRowInput;
    type Output = RunRowOutput;
    type CommandOutput = ();
    type ParentWidget = adw::PreferencesGroup;

    view! {
        adw::ActionRow {
            #[watch]
            set_title: &self.title(),
            #[watch]
            set_subtitle: &self.subtitle(),
            set_subtitle_lines: 1,

            // Activating the row opens the native detail page.
            set_activatable: true,
            connect_activated[sender, id = self.run.id] => move |_| {
                sender.output(RunRowOutput::ShowDetails(id)).ok();
            },

            // The shared status chip. `set_css_classes` replaces the whole list,
            // so the previous variant can't accumulate across updates; the base
            // "status-chip" class rides along to keep the pill and dot styling.
            #[template]
            add_prefix = &StatusChip {
                #[watch]
                set_css_classes: &[
                    "status-chip",
                    status_chip::variant(self.run.status, self.run.conclusion),
                ],
                #[template_child]
                label {
                    #[watch]
                    set_label: status_chip::label(self.run.status, self.run.conclusion),
                },
            },

            // A single-click primary action — cancel while running, re-run once
            // finished — bound straight to the shared "runs" action. Icon-only in
            // the compact list; the detail page gives it a label.
            add_suffix = &gtk::Button {
                set_valign: gtk::Align::Center,
                add_css_class: "flat",
                #[watch]
                set_icon_name: run_action_spec(self.run.status).icon,
                #[watch]
                set_tooltip_text: Some(run_action_spec(self.run.status).label),
                // `set_action_name` clears the target, so re-apply the id after it.
                #[watch]
                set_action_name: Some(run_action_spec(self.run.status).action),
                #[watch]
                set_action_target_value: Some(&self.run.id.to_variant()),
                // After the action binding, so it wins: disable while cancelling.
                #[watch]
                set_sensitive: run_action_spec(self.run.status).enabled,
            },

            // Opens the run on github.com; activating the row itself opens the
            // native detail page.
            add_suffix = &gtk::Button {
                set_icon_name: "web-browser-symbolic",
                set_valign: gtk::Align::Center,
                set_tooltip_text: Some("Open in browser"),
                add_css_class: "flat",
                connect_clicked[sender, url = self.run.html_url.clone()] => move |_| {
                    sender.output(RunRowOutput::OpenInBrowser(url.clone())).ok();
                },
            },
        }
    }

    fn init_model(init: Self::Init, _index: &DynamicIndex, _sender: FactorySender<Self>) -> Self {
        Self { run: init }
    }

    fn update(&mut self, msg: Self::Input, _sender: FactorySender<Self>) {
        match msg {
            // Swapping the data is enough: the `#[watch]` setters re-run against
            // the new value and mutate only the widgets that changed.
            RunRowInput::Update(run) => self.run = run,
        }
    }
}
