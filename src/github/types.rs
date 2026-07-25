// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Our own GitHub types — the boundary octocrab's models stop at (CLAUDE.md
//! rule 4).
//!
//! Mapping octocrab's models into ours here keeps the components and the `view!`
//! macros free of octocrab entirely: `RateLimit`, `Repo`, and the run trio
//! (`WorkflowRun` / `RunStatus` / `Conclusion`). Jobs and steps arrive with the
//! detail page.

use chrono::{DateTime, Utc};
use octocrab::models::Repository;
use octocrab::models::workflows::{
    Conclusion as ApiConclusion, Job as ApiJob, Run, Status as ApiStatus, Step as ApiStep,
};

/// A snapshot of the authenticated **core** rate-limit budget.
///
/// Authenticated REST gives 5000 requests/hour; this is how much of that is
/// left. Read at connect time and refreshed from every poll's `X-RateLimit-*`
/// response headers, to drive the backoff and the readout (CLAUDE.md rule 3).
#[derive(Debug, Clone, Copy)]
pub struct RateLimit {
    pub remaining: u64,
    pub limit: u64,
    /// When the window resets. `None` if the header was missing/unparseable.
    pub reset: Option<DateTime<Utc>>,
}

impl RateLimit {
    /// Whole minutes until the limit resets — 0 if it's already past or unknown.
    /// For the "resets in N min" readout.
    pub fn resets_in_minutes(&self) -> i64 {
        self.reset
            .map(|reset| (reset - Utc::now()).num_minutes().max(0))
            .unwrap_or(0)
    }

    /// Whether the budget is fully spent and the window hasn't reset yet — the
    /// signal to pause polling rather than earn a 403.
    pub fn is_exhausted(&self) -> bool {
        self.remaining == 0 && self.reset.is_some_and(|reset| reset > Utc::now())
    }
}

/// A repository we can watch. octocrab's `Repository` — a swamp of optionals — is
/// narrowed to this at the boundary; everything downstream keys off `owner/name`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    pub owner: String,
    pub name: String,
    pub private: bool,
}

impl Repo {
    /// `owner/name` — GitHub's canonical repo identifier, and how the watch list
    /// is stored (in settings) and keyed (in the picker).
    pub fn full_name(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }

    /// Narrow octocrab's model. Returns `None` for a repo with no owner — it's
    /// unusable, since every API call keys off `owner/name`.
    pub fn from_model(repo: Repository) -> Option<Self> {
        Some(Self {
            owner: repo.owner?.login,
            name: repo.name,
            // The API omits `private` for some responses; absent means public.
            private: repo.private.unwrap_or(false),
        })
    }
}

/// Where a run is in its lifecycle. GitHub's `status` string, narrowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    Queued,
    InProgress,
    Completed,
    /// Client-side only: we've asked GitHub to cancel this run and it hasn't
    /// finished stopping yet. GitHub reports no such status (the run stays
    /// `in_progress` until fully cancelled), so `from_api` / `from_status` never
    /// produce it — the app overlays it for immediate, lasting feedback.
    Cancelling,
    /// A status string we don't recognise — surfaced as-is rather than guessed.
    Unknown,
}

impl RunStatus {
    fn from_api(status: &str) -> Self {
        match status {
            "queued" | "pending" | "waiting" | "requested" => RunStatus::Queued,
            "in_progress" => RunStatus::InProgress,
            "completed" => RunStatus::Completed,
            _ => RunStatus::Unknown,
        }
    }

    /// From octocrab's typed `Status` enum (jobs and steps use it, rather than
    /// the run's string). `#[non_exhaustive]`, hence the catch-all.
    fn from_status(status: &ApiStatus) -> Self {
        match status {
            ApiStatus::Queued | ApiStatus::Pending | ApiStatus::Waiting => RunStatus::Queued,
            ApiStatus::InProgress => RunStatus::InProgress,
            ApiStatus::Completed | ApiStatus::Failed => RunStatus::Completed,
            _ => RunStatus::Unknown,
        }
    }

    /// A run still doing work — the poll should stay attentive to these (adaptive
    /// polling, later), and the header counts them as "running".
    pub fn is_active(self) -> bool {
        matches!(
            self,
            RunStatus::Queued | RunStatus::InProgress | RunStatus::Cancelling
        )
    }
}

/// How a completed run turned out. `None` on a `WorkflowRun` means "not completed
/// yet" (GitHub sends `conclusion: null` until then).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Conclusion {
    Success,
    Failure,
    Cancelled,
    Skipped,
    TimedOut,
    ActionRequired,
    Neutral,
    StartupFailure,
    Stale,
    Unknown,
}

impl Conclusion {
    fn from_api(conclusion: &str) -> Self {
        match conclusion {
            "success" => Conclusion::Success,
            "failure" => Conclusion::Failure,
            "cancelled" => Conclusion::Cancelled,
            "skipped" => Conclusion::Skipped,
            "timed_out" => Conclusion::TimedOut,
            "action_required" => Conclusion::ActionRequired,
            "neutral" => Conclusion::Neutral,
            "startup_failure" => Conclusion::StartupFailure,
            "stale" => Conclusion::Stale,
            _ => Conclusion::Unknown,
        }
    }

    /// From octocrab's typed `Conclusion` enum (jobs and steps). It carries
    /// fewer variants than the run's string form; `#[non_exhaustive]`, hence the
    /// catch-all.
    fn from_conclusion(conclusion: &ApiConclusion) -> Self {
        match conclusion {
            ApiConclusion::Success => Conclusion::Success,
            ApiConclusion::Failure => Conclusion::Failure,
            ApiConclusion::Cancelled => Conclusion::Cancelled,
            ApiConclusion::Skipped => Conclusion::Skipped,
            ApiConclusion::TimedOut => Conclusion::TimedOut,
            ApiConclusion::ActionRequired => Conclusion::ActionRequired,
            ApiConclusion::Neutral => Conclusion::Neutral,
            _ => Conclusion::Unknown,
        }
    }

    /// A red, went-wrong outcome — what a failure notification keys off, and what
    /// the header counts as "failing".
    pub fn is_failure(self) -> bool {
        matches!(
            self,
            Conclusion::Failure | Conclusion::TimedOut | Conclusion::StartupFailure
        )
    }
}

/// One workflow run, the unit the list shows. octocrab's `Run` stops here.
/// `PartialEq` lets the repos view skip rebuilding a repo whose runs are unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowRun {
    /// GitHub's run id — the stable key the list reconciles on.
    pub id: u64,
    pub run_number: i64,
    /// The workflow's name, e.g. "CI".
    pub workflow_name: String,
    /// `owner/name` of the repo this run belongs to.
    pub repo: String,
    pub head_branch: String,
    pub event: String,
    pub status: RunStatus,
    pub conclusion: Option<Conclusion>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// The head commit's SHA (full; the detail page shows the short form).
    pub head_sha: String,
    /// The head commit message (may be multi-line; the UI takes the first line).
    pub commit_message: String,
    /// The head commit's author name.
    pub commit_author: String,
    /// The run's page on github.com — for "open in browser".
    pub html_url: String,
}

impl WorkflowRun {
    /// Narrow octocrab's `Run`. `repo` is the `owner/name` the caller queried —
    /// authoritative, and saves trusting the model's optional repository block.
    pub fn from_model(run: Run, repo: String) -> Self {
        Self {
            id: run.id.into_inner(),
            run_number: run.run_number,
            workflow_name: run.name,
            repo,
            head_branch: run.head_branch,
            event: run.event,
            status: RunStatus::from_api(&run.status),
            conclusion: run.conclusion.as_deref().map(Conclusion::from_api),
            created_at: run.created_at,
            updated_at: run.updated_at,
            head_sha: run.head_sha,
            commit_message: run.head_commit.message,
            commit_author: run.head_commit.author.name,
            html_url: run.html_url.to_string(),
        }
    }
}

/// One job in a run — the unit the detail page lists, each with its steps.
/// `PartialEq` lets the detail skip rebuilding when a refresh brings no change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    /// GitHub's job id — the key to the per-job log endpoint.
    pub id: u64,
    pub name: String,
    pub status: RunStatus,
    pub conclusion: Option<Conclusion>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub steps: Vec<Step>,
    /// The job's **check-run** id (parsed from its `check_run_url`) — the key to
    /// its annotations on the Checks API. `None` if the URL didn't parse.
    pub check_run_id: Option<u64>,
}

impl Job {
    pub fn from_model(job: ApiJob) -> Self {
        Self {
            id: job.id.into_inner(),
            name: job.name,
            status: RunStatus::from_status(&job.status),
            conclusion: job.conclusion.map(|c| Conclusion::from_conclusion(&c)),
            started_at: Some(job.started_at),
            completed_at: job.completed_at,
            steps: job.steps.into_iter().map(Step::from_model).collect(),
            check_run_id: parse_trailing_id(&job.check_run_url),
        }
    }
}

/// The trailing numeric id of a `.../check-runs/{id}` URL, if present.
fn parse_trailing_id(url: &str) -> Option<u64> {
    url.rsplit('/').next()?.parse().ok()
}

/// How serious an annotation is — GitHub's `annotation_level`, narrowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnotationLevel {
    Failure,
    Warning,
    Notice,
}

impl AnnotationLevel {
    /// Unknown levels read as warnings — the safe middle.
    pub fn from_api(level: Option<&str>) -> Self {
        match level {
            Some("failure") => AnnotationLevel::Failure,
            Some("notice") => AnnotationLevel::Notice,
            _ => AnnotationLevel::Warning,
        }
    }

    /// Sort key: failures first, then warnings, then notices.
    pub fn order(self) -> u8 {
        match self {
            AnnotationLevel::Failure => 0,
            AnnotationLevel::Warning => 1,
            AnnotationLevel::Notice => 2,
        }
    }
}

/// A single annotation on a run — a warning / error / notice GitHub surfaces in
/// its "Annotations" section, with an optional file location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Annotation {
    pub level: AnnotationLevel,
    pub title: Option<String>,
    pub message: String,
    /// `path:line`, or empty when the annotation isn't file-bound.
    pub location: String,
    /// The job it came from, for context.
    pub job: String,
}

impl Annotation {
    /// The line shown as the row's title: the message, or the title as a fallback.
    pub fn headline(&self) -> &str {
        if !self.message.is_empty() {
            &self.message
        } else {
            self.title.as_deref().unwrap_or("(no message)")
        }
    }

    /// "src/foo.rs:12 · build" — location and job, whichever are present.
    pub fn context(&self) -> String {
        match (self.location.is_empty(), self.job.is_empty()) {
            (false, false) => format!("{} · {}", self.location, self.job),
            (true, false) => self.job.clone(),
            (false, true) => self.location.clone(),
            (true, true) => String::new(),
        }
    }
}

/// One step within a job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub name: String,
    pub status: RunStatus,
    pub conclusion: Option<Conclusion>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

impl Step {
    pub fn from_model(step: ApiStep) -> Self {
        Self {
            name: step.name,
            status: RunStatus::from_status(&step.status),
            conclusion: step.conclusion.map(|c| Conclusion::from_conclusion(&c)),
            started_at: step.started_at,
            completed_at: step.completed_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_name_is_owner_slash_name() {
        let repo = Repo {
            owner: "SoftARV".to_owned(),
            name: "Pitwall".to_owned(),
            private: true,
        };
        assert_eq!(repo.full_name(), "SoftARV/Pitwall");
    }

    #[test]
    fn run_status_maps_the_api_strings() {
        assert_eq!(RunStatus::from_api("queued"), RunStatus::Queued);
        assert_eq!(RunStatus::from_api("waiting"), RunStatus::Queued);
        assert_eq!(RunStatus::from_api("in_progress"), RunStatus::InProgress);
        assert_eq!(RunStatus::from_api("completed"), RunStatus::Completed);
        assert_eq!(RunStatus::from_api("nonsense"), RunStatus::Unknown);
        assert!(RunStatus::InProgress.is_active());
        assert!(!RunStatus::Completed.is_active());
    }

    #[test]
    fn conclusion_maps_and_flags_failures() {
        assert_eq!(Conclusion::from_api("success"), Conclusion::Success);
        assert_eq!(Conclusion::from_api("timed_out"), Conclusion::TimedOut);
        assert_eq!(Conclusion::from_api("nonsense"), Conclusion::Unknown);
        assert!(Conclusion::Failure.is_failure());
        assert!(Conclusion::TimedOut.is_failure());
        assert!(!Conclusion::Success.is_failure());
        assert!(!Conclusion::Cancelled.is_failure());
    }

    #[test]
    fn maps_octocrab_job_enums() {
        assert_eq!(
            RunStatus::from_status(&ApiStatus::InProgress),
            RunStatus::InProgress
        );
        assert_eq!(
            RunStatus::from_status(&ApiStatus::Queued),
            RunStatus::Queued
        );
        assert_eq!(
            RunStatus::from_status(&ApiStatus::Completed),
            RunStatus::Completed
        );
        assert_eq!(
            Conclusion::from_conclusion(&ApiConclusion::Success),
            Conclusion::Success
        );
        assert_eq!(
            Conclusion::from_conclusion(&ApiConclusion::TimedOut),
            Conclusion::TimedOut
        );
    }
}
