// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! The run detail page — a run's metadata plus its jobs and steps.
//!
//! A relm4 child `Component` whose root is an `adw::NavigationPage`, pushed onto
//! the app's `NavigationView`. It shows what we already know about the run (from
//! the list), then fetches its jobs once and lists each as an expandable row of
//! steps. octocrab is used only to fetch; the UI sees our types (rule 4).
//!
//! The page stays live while it's open: the app forwards a `Refresh` (the latest
//! run + a re-fetch of its jobs) on every poll and after a run action, so the
//! metadata and jobs track reality the same way the list does. Jobs are rebuilt
//! only when they actually change, and expanded rows stay expanded across a
//! refresh.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use relm4::adw::prelude::*;
use relm4::gtk::glib;
use relm4::{Component, ComponentParts, ComponentSender, RelmWidgetExt, adw, gtk};

use octocrab::Octocrab;

use crate::components::run_row::run_action_spec;
use crate::components::status_chip::{self, StatusChip};
use crate::github::client;
use crate::github::types::{
    Annotation, AnnotationLevel, Conclusion, Job, RunStatus, Step, WorkflowRun,
};

pub struct RunDetailInit {
    pub octocrab: Octocrab,
    pub run: WorkflowRun,
}

pub struct RunDetail {
    run: WorkflowRun,
    state: JobsState,
    /// Kept so refreshes can re-fetch without the app re-plumbing it (Arc-backed,
    /// so the clone is a pointer bump).
    octocrab: Octocrab,
    /// Held so the load handler can fill it once the jobs arrive — it's built
    /// empty and populated imperatively (the rows are dynamic).
    jobs_group: adw::PreferencesGroup,
    /// The jobs currently shown — a refresh rebuilds only when this changes.
    jobs: Vec<Job>,
    /// The built job rows (by job name), for clearing on rebuild and for
    /// preserving which were expanded across it.
    job_rows: Vec<(String, adw::ExpanderRow)>,
    /// The "Annotations" group (warnings / errors / notices), hidden until there
    /// are any. Filled from the jobs' check runs when the jobs change.
    annotations_group: adw::PreferencesGroup,
    /// The built annotation rows, for clearing on refresh.
    annotation_rows: Vec<adw::ActionRow>,
}

#[derive(Debug)]
pub enum RunDetailInput {
    /// Fresh run data plus a trigger to re-fetch its jobs — sent by the app on
    /// each poll and after a run action, to keep this page live.
    Refresh(Box<WorkflowRun>),
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
    /// A run's annotations, fetched after the jobs settle.
    AnnotationsLoaded(Vec<Annotation>),
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

    /// Re-fetch this run's jobs off-thread; the result lands as `JobsLoaded`.
    /// Shared by the initial load and every refresh.
    fn fetch_jobs(&self, sender: &ComponentSender<Self>) {
        let octocrab = self.octocrab.clone();
        let repo = self.run.repo.clone();
        let run_id = self.run.id;
        sender.oneshot_command(async move {
            let outcome = match repo.split_once('/') {
                Some((owner, name)) => client::list_jobs(&octocrab, owner, name, run_id).await,
                None => Err(format!("“{repo}” isn't a valid owner/name")),
            };
            RunDetailCmd::JobsLoaded(Box::new(outcome))
        });
    }

    /// Fetch the run's annotations off-thread from the (current) jobs' check runs;
    /// the result lands as `AnnotationsLoaded`. Called only when the jobs change,
    /// so it doesn't re-run on every poll.
    fn fetch_annotations(&self, sender: &ComponentSender<Self>) {
        let octocrab = self.octocrab.clone();
        let repo = self.run.repo.clone();
        let jobs = self.jobs.clone();
        sender.oneshot_command(async move {
            let annotations = match repo.split_once('/') {
                Some((owner, name)) => {
                    client::fetch_annotations(&octocrab, owner, name, &jobs).await
                }
                None => Vec::new(),
            };
            RunDetailCmd::AnnotationsLoaded(annotations)
        });
    }

    /// Fill the annotations group, hiding it when there are none. Rebuilt whole
    /// each time (annotations are few and flat — no expansion to preserve).
    fn populate_annotations(&mut self, annotations: &[Annotation]) {
        for row in self.annotation_rows.drain(..) {
            self.annotations_group.remove(&row);
        }
        for annotation in annotations {
            let row = adw::ActionRow::new();
            row.set_title(&glib::markup_escape_text(annotation.headline()));
            row.set_title_lines(2);
            let context = annotation.context();
            if !context.is_empty() {
                row.set_subtitle(&glib::markup_escape_text(&context));
            }
            row.add_prefix(&annotation_icon(annotation.level));
            self.annotations_group.add(&row);
            self.annotation_rows.push(row);
        }
        self.annotations_group.set_visible(!annotations.is_empty());
    }

    /// Rebuild the jobs group: one expandable row per job, each expanding to its
    /// steps. On a refresh the previous rows are removed first and their expanded
    /// state restored by job name, so watching a job's steps isn't interrupted.
    /// Each job's "View log" button emits `ShowJobLog` so the app can push its
    /// log page.
    fn populate_jobs(&mut self, jobs: &[Job], sender: &ComponentSender<Self>) {
        // Remember which jobs the user had expanded, then clear the old rows.
        let expanded: HashSet<String> = self
            .job_rows
            .iter()
            .filter(|(_, row)| row.is_expanded())
            .map(|(name, _)| name.clone())
            .collect();
        for (_, row) in self.job_rows.drain(..) {
            self.jobs_group.remove(&row);
        }

        for job in jobs {
            let row = adw::ExpanderRow::new();
            row.set_title(&job.name);
            row.set_subtitle(&job_duration(job));
            row.set_expanded(expanded.contains(&job.name));

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
            self.job_rows.push((job.name.clone(), row));
        }
    }
}

#[relm4::component(pub)]
impl Component for RunDetail {
    type Init = RunDetailInit;
    type Input = RunDetailInput;
    type Output = RunDetailOutput;
    type CommandOutput = RunDetailCmd;

    view! {
        adw::NavigationPage {
            #[watch]
            set_title: &model.title(),

            adw::ToolbarView {
                add_top_bar = &adw::HeaderBar {
                    // The back button is added by the NavigationView automatically.
                    // Single-click primary action (cancel / re-run), labelled. It
                    // reuses the shared "runs" actions (resolved on the root
                    // window). `#[watch]` keeps it in step as the run transitions
                    // (e.g. cancel → re-run) while the page is open.
                    pack_end = &gtk::Button {
                        #[watch]
                        set_tooltip_text: Some(run_action_spec(model.run.status).label),
                        // `set_action_name` clears the target, so re-apply it after.
                        #[watch]
                        set_action_name: Some(run_action_spec(model.run.status).action),
                        #[watch]
                        set_action_target_value: Some(&model.run.id.to_variant()),
                        // After the action binding, so it wins: disable while cancelling.
                        #[watch]
                        set_sensitive: run_action_spec(model.run.status).enabled,
                        #[wrap(Some)]
                        set_child = &adw::ButtonContent {
                            #[watch]
                            set_icon_name: run_action_spec(model.run.status).icon,
                            #[watch]
                            set_label: run_action_spec(model.run.status).label,
                        },
                    },
                    // Re-run only the failed jobs — shown when the run failed.
                    pack_end = &gtk::Button {
                        #[watch]
                        set_visible: model.run.conclusion.is_some_and(Conclusion::is_failure),
                        set_tooltip_text: Some("Re-run only the failed jobs"),
                        set_action_name: Some("runs.rerun-failed"),
                        set_action_target_value: Some(&model.run.id.to_variant()),
                        #[wrap(Some)]
                        set_child = &adw::ButtonContent {
                            set_icon_name: "view-refresh-symbolic",
                            set_label: "Re-run failed",
                        },
                    },
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
                                        #[watch]
                                        set_css_classes: &[
                                            "status-chip",
                                            status_chip::variant(model.run.status, model.run.conclusion),
                                        ],
                                        #[template_child]
                                        label {
                                            #[watch]
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

                            // Warnings / errors / notices from the run's check
                            // runs. Filled (and shown) by `populate_annotations`.
                            #[local_ref]
                            annotations_group -> adw::PreferencesGroup {
                                set_title: "Annotations",
                                set_visible: false,
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
        let annotations_group = adw::PreferencesGroup::new();

        let model = RunDetail {
            run: init.run,
            state: JobsState::Loading,
            octocrab: init.octocrab,
            jobs_group: jobs_group.clone(),
            jobs: Vec::new(),
            job_rows: Vec::new(),
            annotations_group: annotations_group.clone(),
            annotation_rows: Vec::new(),
        };

        let widgets = view_output!();
        model.fetch_jobs(&sender);
        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>, _root: &Self::Root) {
        match msg {
            // Latest run data drives the `#[watch]` metadata (status, elapsed, the
            // action button); re-fetch the jobs to match.
            RunDetailInput::Refresh(run) => {
                self.run = *run;
                self.fetch_jobs(&sender);
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
            RunDetailCmd::JobsLoaded(result) => match *result {
                Ok(jobs) => {
                    // Rebuild only on a real change, so an unchanged poll doesn't
                    // flicker the list or collapse an expanded job.
                    if jobs != self.jobs {
                        self.populate_jobs(&jobs, &sender);
                        self.jobs = jobs;
                        // Annotations follow the jobs — fetch only when they move.
                        self.fetch_annotations(&sender);
                    }
                    self.state = JobsState::Loaded;
                }
                // A failed *refresh* keeps what we have; only a failed first load
                // (nothing shown yet) becomes the error state.
                Err(message) => {
                    if self.jobs.is_empty() {
                        self.state = JobsState::Error(message);
                    }
                }
            },
            RunDetailCmd::AnnotationsLoaded(annotations) => {
                self.populate_annotations(&annotations);
            }
        }
    }
}

/// A coloured level icon for an annotation row.
fn annotation_icon(level: AnnotationLevel) -> gtk::Image {
    let (icon, css) = match level {
        AnnotationLevel::Failure => ("dialog-error-symbolic", "error"),
        AnnotationLevel::Warning => ("dialog-warning-symbolic", "warning"),
        AnnotationLevel::Notice => ("dialog-information-symbolic", "accent"),
    };
    let image = gtk::Image::from_icon_name(icon);
    image.add_css_class(css);
    image
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
