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
use octocrab::models::workflows::Run;

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

    /// A run still doing work — the poll should stay attentive to these (adaptive
    /// polling, later), and the header counts them as "running".
    pub fn is_active(self) -> bool {
        matches!(self, RunStatus::Queued | RunStatus::InProgress)
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
#[derive(Debug, Clone)]
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
            html_url: run.html_url.to_string(),
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
}
