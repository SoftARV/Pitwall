// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! The run detail page — a run's metadata plus its jobs and steps.
//!
//! A relm4 child `Component` whose root is an `adw::NavigationPage`, pushed onto
//! the app's `NavigationView`. It shows what we already know about the run (from
//! the list), then fetches its jobs once and lists each as an expandable row of
//! steps. octocrab is used only to fetch; the UI sees our types (rule 4).
//!
//! Jobs are fetched once, on open — not polled. A run still in progress is a
//! snapshot; reopen (or "Open in Browser") for live state. Live job polling is a
//! deliberate later refinement.

use chrono::{DateTime, Utc};
use relm4::adw::prelude::*;
use relm4::{Component, ComponentParts, ComponentSender, RelmWidgetExt, adw, gtk};

use octocrab::Octocrab;

use crate::components::status_chip::{self, StatusChip};
use crate::github::client;
use crate::github::types::{Job, RunStatus, Step, WorkflowRun};

pub struct RunDetailInit {
    pub octocrab: Octocrab,
    pub run: WorkflowRun,
}

pub struct RunDetail {
    run: WorkflowRun,
    state: JobsState,
    /// Held so the load handler can fill it once the jobs arrive — it's built
    /// empty and populated imperatively (the rows are dynamic).
    jobs_group: adw::PreferencesGroup,
}

#[derive(Debug)]
enum JobsState {
    Loading,
    Loaded,
    Error(String),
}

#[derive(Debug)]
pub enum RunDetailOutput {
    /// Open this run on github.com (the header button).
    OpenInBrowser(String),
    /// A job's "View log" was activated — open its log page. Carries what the
    /// log view needs to fetch and render the whole job log.
    ShowJobLog {
        repo: String,
        job_id: u64,
        /// The job's name — the log page's title.
        job_name: String,
        /// Logs exist only for a finished job; a running one shows a placeholder.
        completed: bool,
        html_url: String,
    },
}

#[derive(Debug)]
pub enum RunDetailCmd {
    JobsLoaded(Box<Result<Vec<Job>, String>>),
}

impl RunDetail {
    /// "CI #128" — the page title.
    fn title(&self) -> String {
        format!("{} #{}", self.run.workflow_name, self.run.run_number)
    }

    /// The first line of the commit message, for the commit row's title.
    fn commit_title(&self) -> &str {
        self.run.commit_message.lines().next().unwrap_or("")
    }

    /// "a1b2c3d · Jane Doe" — short SHA and author, for the commit row subtitle.
    fn commit_subtitle(&self) -> String {
        let short = self.run.head_sha.get(..7).unwrap_or(&self.run.head_sha);
        format!("{short} · {}", self.run.commit_author)
    }

    /// "2026-07-24 14:30 · 1m 23s" — start time and elapsed.
    fn started_text(&self) -> String {
        let when = self.run.created_at.format("%Y-%m-%d %H:%M").to_string();
        let end = if self.run.status == RunStatus::Completed {
            self.run.updated_at
        } else {
            Utc::now()
        };
        format!("{when} · {}", format_duration(self.run.created_at, end))
    }

    fn jobs_page(&self) -> &'static str {
        match self.state {
            JobsState::Loading => "loading",
            JobsState::Loaded => "jobs",
            JobsState::Error(_) => "error",
        }
    }

    fn error(&self) -> String {
        match &self.state {
            JobsState::Error(message) => message.clone(),
            _ => String::new(),
        }
    }

    /// Fill the jobs group with one expandable row per job, each expanding to its
    /// steps. Built imperatively (dynamic count); the chips use the shared
    /// `status_chip::build`. Called once, so there's nothing to clear first.
    /// Each job's "View log" button emits `ShowJobLog` so the app can push its
    /// log page.
    fn populate_jobs(&self, jobs: &[Job], sender: &ComponentSender<Self>) {
        for job in jobs {
            let row = adw::ExpanderRow::new();
            row.set_title(&job.name);
            row.set_subtitle(&job_duration(job));

            // "View log" opens the whole job's log. It's a plain button, so
            // clicking it doesn't toggle the expander — only the row's title area
            // does. Placed before the chip so the status chip stays rightmost.
            let log_button = gtk::Button::from_icon_name("utilities-terminal-symbolic");
            log_button.set_tooltip_text(Some("View log"));
            log_button.set_valign(gtk::Align::Center);
            log_button.add_css_class("flat");
            let output = sender.output_sender().clone();
            let repo = self.run.repo.clone();
            let html_url = self.run.html_url.clone();
            let job_name = job.name.clone();
            let job_id = job.id;
            let completed = job.status == RunStatus::Completed;
            log_button.connect_clicked(move |_| {
                output
                    .send(RunDetailOutput::ShowJobLog {
                        repo: repo.clone(),
                        job_id,
                        job_name: job_name.clone(),
                        completed,
                        html_url: html_url.clone(),
                    })
                    .ok();
            });
            row.add_suffix(&log_button);
            row.add_suffix(&status_chip::build(job.status, job.conclusion));

            // Steps are informational: status chip + duration, not clickable.
            for step in &job.steps {
                let step_row = adw::ActionRow::new();
                step_row.set_title(&step.name);
                step_row.set_subtitle(&step_duration(step));
                step_row.add_prefix(&status_chip::build(step.status, step.conclusion));
                row.add_row(&step_row);
            }
            self.jobs_group.add(&row);
        }
    }
}

#[relm4::component(pub)]
impl Component for RunDetail {
    type Init = RunDetailInit;
    type Input = ();
    type Output = RunDetailOutput;
    type CommandOutput = RunDetailCmd;

    view! {
        adw::NavigationPage {
            #[watch]
            set_title: &model.title(),

            adw::ToolbarView {
                add_top_bar = &adw::HeaderBar {
                    // The back button is added by the NavigationView automatically.
                    pack_end = &gtk::Button {
                        set_icon_name: "web-browser-symbolic",
                        set_tooltip_text: Some("Open in browser"),
                        connect_clicked[sender, url = model.run.html_url.clone()] => move |_| {
                            sender.output(RunDetailOutput::OpenInBrowser(url.clone())).ok();
                        },
                    },
                },

                #[wrap(Some)]
                set_content = &gtk::ScrolledWindow {
                    set_vexpand: true,

                    adw::Clamp {
                        set_maximum_size: 700,
                        set_margin_all: 12,

                        gtk::Box {
                            set_orientation: gtk::Orientation::Vertical,
                            set_spacing: 18,

                            adw::PreferencesGroup {
                                set_title: "Run",

                                adw::ActionRow {
                                    set_title: "Status",
                                    #[template]
                                    add_suffix = &StatusChip {
                                        set_css_classes: &[
                                            "status-chip",
                                            status_chip::variant(model.run.status, model.run.conclusion),
                                        ],
                                        #[template_child]
                                        label {
                                            set_label: status_chip::label(model.run.status, model.run.conclusion),
                                        },
                                    },
                                },
                                adw::ActionRow {
                                    set_title: "Repository",
                                    set_subtitle: &model.run.repo,
                                },
                                adw::ActionRow {
                                    set_title: "Branch",
                                    set_subtitle: &model.run.head_branch,
                                },
                                adw::ActionRow {
                                    set_title: "Event",
                                    set_subtitle: &model.run.event,
                                },
                                adw::ActionRow {
                                    set_title: "Started",
                                    #[watch]
                                    set_subtitle: &model.started_text(),
                                },
                            },

                            adw::PreferencesGroup {
                                set_title: "Commit",

                                adw::ActionRow {
                                    set_title: model.commit_title(),
                                    set_subtitle: &model.commit_subtitle(),
                                },
                            },

                            gtk::Stack {
                                add_named[Some("loading")] = &gtk::Box {
                                    set_orientation: gtk::Orientation::Vertical,
                                    set_valign: gtk::Align::Center,
                                    set_halign: gtk::Align::Center,
                                    set_margin_all: 24,
                                    gtk::Spinner {
                                        set_spinning: true,
                                        set_size_request: (32, 32),
                                    },
                                },

                                add_named[Some("jobs")] = &gtk::Box {
                                    set_orientation: gtk::Orientation::Vertical,

                                    #[local_ref]
                                    jobs_group -> adw::PreferencesGroup {
                                        set_title: "Jobs",
                                    },
                                },

                                add_named[Some("error")] = &adw::StatusPage {
                                    set_icon_name: Some("network-error-symbolic"),
                                    set_title: "Couldn't load jobs",
                                    #[watch]
                                    set_description: Some(&model.error()),
                                },

                                #[watch]
                                set_visible_child_name: model.jobs_page(),
                            },
                        },
                    },
                },
            },
        }
    }

    fn init(
        init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let jobs_group = adw::PreferencesGroup::new();

        // Capture what the fetch needs before the run moves into the model.
        let repo = init.run.repo.clone();
        let run_id = init.run.id;
        let octocrab = init.octocrab;

        let model = RunDetail {
            run: init.run,
            state: JobsState::Loading,
            jobs_group: jobs_group.clone(),
        };

        let widgets = view_output!();

        sender.oneshot_command(async move {
            let outcome = match repo.split_once('/') {
                Some((owner, name)) => client::list_jobs(&octocrab, owner, name, run_id).await,
                None => Err(format!("“{repo}” isn't a valid owner/name")),
            };
            RunDetailCmd::JobsLoaded(Box::new(outcome))
        });

        ComponentParts { model, widgets }
    }

    fn update_cmd(
        &mut self,
        msg: Self::CommandOutput,
        sender: ComponentSender<Self>,
        _root: &Self::Root,
    ) {
        match msg {
            RunDetailCmd::JobsLoaded(result) => match *result {
                Ok(jobs) => {
                    self.populate_jobs(&jobs, &sender);
                    self.state = JobsState::Loaded;
                }
                Err(message) => {
                    self.state = JobsState::Error(message);
                }
            },
        }
    }
}

/// "45s" / "1m 23s" between two instants (clamped at zero).
fn format_duration(from: DateTime<Utc>, to: DateTime<Utc>) -> String {
    let seconds = (to - from).num_seconds().max(0);
    if seconds < 60 {
        format!("{seconds}s")
    } else {
        let (minutes, rest) = (seconds / 60, seconds % 60);
        if rest == 0 {
            format!("{minutes}m")
        } else {
            format!("{minutes}m {rest}s")
        }
    }
}

fn job_duration(job: &Job) -> String {
    duration_between(job.started_at, job.completed_at)
}

fn step_duration(step: &Step) -> String {
    duration_between(step.started_at, step.completed_at)
}

/// The elapsed text for a job/step given its start and (maybe) end.
fn duration_between(started: Option<DateTime<Utc>>, completed: Option<DateTime<Utc>>) -> String {
    match (started, completed) {
        (Some(start), Some(end)) => format_duration(start, end),
        (Some(start), None) => format!("running · {}", format_duration(start, Utc::now())),
        _ => String::new(),
    }
}
