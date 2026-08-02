// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! The GitHub client: build, verify, and diagnose.
//!
//! CLAUDE.md rule 1: octocrab's API shifts between versions, so every call here
//! was checked against octocrab 0.54's own source, not memory. Rule 4: octocrab
//! types are mapped into ours (`RateLimit`) before leaving this module.

use std::collections::HashMap;

use chrono::DateTime;
use http::header::{ACCEPT, ETAG, IF_NONE_MATCH};
use http::{HeaderMap, HeaderValue};
use octocrab::{FromResponse, Octocrab, Page};
use secrecy::{ExposeSecret, SecretString};

use crate::github::types::{
    Annotation, AnnotationLevel, Job, RateLimit, Repo, RunStatus, WorkflowRun,
};

/// The OAuth App's **public** client id. Device flow needs no secret, and every
/// device-flow client ships its id in the binary (gh's is public too), so
/// committing it is fine. Registered on GitHub with "Enable Device Flow" on.
pub const CLIENT_ID: &str = "Ov23linyDdWA8pzgyrUQ";

/// The scopes we request. `repo` covers reading Actions runs on private and
/// public repos (and, later, re-run / cancel). GitHub has no Actions-only OAuth
/// scope, so this is the narrowest that works — see CLAUDE.md rule 2.
const SCOPES: [&str; 1] = ["repo"];

/// A live, verified connection to GitHub: the octocrab client, who it
/// authenticated as, and how much rate budget is left.
///
/// `Octocrab` is an `Arc`-backed handle (like a database connection pool), so
/// cloning it is a cheap pointer bump — which is exactly what the background
/// poll will need from the run-list milestone on, because an async task must
/// own `'static` data and so can't borrow `&self`.
pub struct Connection {
    /// The client every request goes through — cloned (a cheap Arc bump) into
    /// the repo picker now, and the poll later.
    pub octocrab: Octocrab,
    pub login: String,
    pub rate: RateLimit,
}

// `Octocrab` doesn't implement `Debug`, but relm4's command channel wants our
// `Connection` to (it rides inside `CommandMsg`). Hand-roll it, printing the
// parts we can and eliding the client — which also guarantees the token buried
// inside it can never end up in a log line.
impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection")
            .field("login", &self.login)
            .field("rate", &self.rate)
            .finish_non_exhaustive()
    }
}

/// Why a connection attempt failed, sorted into the cases the UI treats
/// differently: an **auth** failure drops you back to the token screen;
/// everything else lands on a "disconnected" page with the reason. Each message
/// *names the fix* (CLAUDE.md rule 2).
#[derive(Debug)]
pub enum ConnectError {
    /// 401 or a missing token — invalid or expired.
    Auth(String),
    /// The network is down, or GitHub is unreachable.
    Offline(String),
    /// 403 — rate limit reached, or the token lacks access.
    RateLimited(String),
    /// Anything else, surfaced rather than swallowed.
    Other(String),
}

impl ConnectError {
    pub fn message(&self) -> &str {
        match self {
            ConnectError::Auth(m)
            | ConnectError::Offline(m)
            | ConnectError::RateLimited(m)
            | ConnectError::Other(m) => m,
        }
    }

    /// Auth failures are the only ones that should return to the token entry.
    pub fn is_auth(&self) -> bool {
        matches!(self, ConnectError::Auth(_))
    }
}

/// Build a client for `token` and prove it works before returning it.
///
/// `builder().build()` is *lazy* — it constructs a client without making a
/// request, so it would happily hand back a client for a dead or revoked token.
/// So, exactly like Dockyard's `ping()`, we make one cheap authenticated call
/// (`/user`) and read the rate limit before declaring success: a present-but-
/// dead token must never render an empty, healthy-looking app (CLAUDE.md rule 2).
pub async fn connect(token: String) -> Result<Connection, ConnectError> {
    // `build()` is synchronous in 0.54 (no `.await`) and only fails on a
    // malformed client config, never on anything network-related.
    let octocrab = Octocrab::builder()
        .personal_token(token)
        .build()
        .map_err(diagnose)?;

    let user = octocrab.current().user().await.map_err(diagnose)?;
    let limits = octocrab.ratelimit().get().await.map_err(diagnose)?;

    Ok(Connection {
        octocrab,
        login: user.login,
        // octocrab reports these as `usize`; widen to `u64` so our type doesn't
        // carry a platform-dependent width into the rest of the app.
        rate: RateLimit {
            remaining: limits.resources.core.remaining as u64,
            limit: limits.resources.core.limit as u64,
            reset: DateTime::from_timestamp(limits.resources.core.reset as i64, 0),
        },
    })
}

/// List the repositories the token can watch: the ones the user owns,
/// collaborates on, or can see through org membership. Newest-activity-first
/// (`sort=pushed`), so the repos you're likely to want sit at the top of the
/// picker.
///
/// `all_pages` walks GitHub's `Link` headers, so a user with more than 100 repos
/// still gets the whole set — a handful of requests, once, on demand (not on the
/// poll path, so it doesn't compete with the rate budget the poll guards).
pub async fn list_repos(octocrab: &Octocrab) -> Result<Vec<Repo>, ConnectError> {
    let first = octocrab
        .current()
        .list_repos_for_authenticated_user()
        .affiliation("owner,collaborator,organization_member")
        .sort("pushed")
        .per_page(100)
        .send()
        .await
        .map_err(diagnose)?;
    let all = octocrab.all_pages(first).await.map_err(diagnose)?;
    Ok(all.into_iter().filter_map(Repo::from_model).collect())
}

/// The outcome of polling one repo's runs.
#[derive(Debug)]
pub enum RunsOutcome {
    /// New data (HTTP 200): the runs, plus the ETag to send next time.
    Fresh {
        etag: Option<String>,
        runs: Vec<WorkflowRun>,
        /// Set when the repo's **current** `full_name` differs from the one we
        /// queried — i.e. it was renamed on GitHub and our request followed the
        /// redirect. The caller migrates its watchlist and caches to this name.
        renamed_to: Option<String>,
    },
    /// 304 Not Modified — nothing changed, and it did **not** cost a request
    /// against the rate limit. This is what makes idle repos free to poll.
    NotModified,
    /// The request failed (404, offline, …); the message names the fix.
    Failed(String),
}

/// One repo's slot in a poll.
#[derive(Debug)]
pub struct RepoRuns {
    pub repo: String,
    pub outcome: RunsOutcome,
}

/// The result of one poll across the watched repos.
#[derive(Debug)]
pub struct Poll {
    pub repos: Vec<RepoRuns>,
    /// The budget after the last response that carried the `X-RateLimit-*`
    /// headers (they ride on 304s too).
    pub rate: Option<RateLimit>,
}

/// Poll every watched repo's runs **conditionally**: send the stored ETag as
/// `If-None-Match`, so an unchanged repo answers `304 Not Modified` — which
/// costs nothing against the rate limit (CLAUDE.md rule 3). The caller keeps the
/// ETags and the per-repo runs between polls; this reports only what changed.
pub async fn poll_runs(
    octocrab: &Octocrab,
    repos: &[String],
    etags: &HashMap<String, String>,
) -> Poll {
    let mut results = Vec::with_capacity(repos.len());
    let mut rate = None;

    for repo in repos {
        let outcome = match repo.split_once('/') {
            Some((owner, name)) => {
                match fetch_runs(octocrab, owner, name, etags.get(repo).map(String::as_str)).await {
                    Ok((outcome, response_rate)) => {
                        // Keep the freshest budget; the headers ride on 304s too.
                        if response_rate.is_some() {
                            rate = response_rate;
                        }
                        outcome
                    }
                    Err(error) => RunsOutcome::Failed(error),
                }
            }
            None => RunsOutcome::Failed(format!("“{repo}” isn't a valid owner/name")),
        };
        results.push(RepoRuns {
            repo: repo.clone(),
            outcome,
        });
    }

    Poll {
        repos: results,
        rate,
    }
}

/// One repo's conditional runs request, dropped to the low level so we can send
/// `If-None-Match` and read the `ETag` / `X-RateLimit-*` headers — octocrab's
/// typed builder exposes none of that.
async fn fetch_runs(
    octocrab: &Octocrab,
    owner: &str,
    name: &str,
    etag: Option<&str>,
) -> Result<(RunsOutcome, Option<RateLimit>), String> {
    let uri = format!("/repos/{owner}/{name}/actions/runs?per_page=20");

    let mut headers = HeaderMap::new();
    if let Some(etag) = etag
        && let Ok(value) = HeaderValue::from_str(etag)
    {
        headers.insert(IF_NONE_MATCH, value);
    }

    let response = octocrab
        ._get_with_headers(uri, Some(headers))
        .await
        .map_err(|err| diagnose(err).message().to_owned())?;

    // `_get_with_headers` returns the raw response — the typed path's
    // error-mapping isn't applied — so a 304 arrives as a plain response for us
    // to branch on rather than as an error.
    let status = response.status().as_u16();
    let rate = read_rate(response.headers());

    match status {
        304 => Ok((RunsOutcome::NotModified, rate)),
        200 => {
            let etag = response
                .headers()
                .get(ETAG)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            // Parse with octocrab's own `Page<Run>` deserialiser (it knows the
            // `workflow_runs` wrapper), then narrow to our type.
            let page = Page::<octocrab::models::workflows::Run>::from_response(response)
                .await
                .map_err(|err| diagnose(err).message().to_owned())?;

            // A renamed repo still answers on its old path (GitHub redirects), so
            // the only way to notice is the repository block each run carries: if
            // its `full_name` isn't what we asked for, the repo was renamed. The
            // comparison is case-insensitive so a differently-cased watchlist
            // entry doesn't masquerade as a rename.
            let queried = format!("{owner}/{name}");
            let current = page
                .items
                .first()
                .and_then(|run| run.repository.full_name.clone())
                .filter(|full| !full.is_empty());
            let renamed_to = current
                .as_deref()
                .filter(|full| !full.eq_ignore_ascii_case(&queried))
                .map(str::to_owned);
            // Label the runs with the repo's real name, so the UI shows where they
            // came from even on the poll that discovers the rename.
            let label = current.unwrap_or_else(|| queried.clone());

            let runs = page
                .items
                .into_iter()
                .map(|run| WorkflowRun::from_model(run, label.clone()))
                .collect();
            Ok((
                RunsOutcome::Fresh {
                    etag,
                    runs,
                    renamed_to,
                },
                rate,
            ))
        }
        code => Err(format!("GitHub returned HTTP {code} for {owner}/{name}")),
    }
}

/// Read the `X-RateLimit-*` budget from a response's headers, if present.
fn read_rate(headers: &HeaderMap) -> Option<RateLimit> {
    let number = |name: &str| -> Option<u64> { headers.get(name)?.to_str().ok()?.parse().ok() };
    Some(RateLimit {
        remaining: number("x-ratelimit-remaining")?,
        limit: number("x-ratelimit-limit")?,
        reset: number("x-ratelimit-reset").and_then(|ts| DateTime::from_timestamp(ts as i64, 0)),
    })
}

/// List the jobs of one run (each carries its steps), for the detail page. One
/// request, on demand — not on the poll path.
pub async fn list_jobs(
    octocrab: &Octocrab,
    owner: &str,
    name: &str,
    run_id: u64,
) -> Result<Vec<Job>, String> {
    let page = octocrab
        .workflows(owner, name)
        .list_jobs(octocrab::models::RunId(run_id))
        .per_page(100u8)
        .send()
        .await
        .map_err(|err| diagnose(err).message().to_owned())?;
    Ok(page.items.into_iter().map(Job::from_model).collect())
}

/// Download one job's full log as raw text — timestamps, ANSI colours and
/// `##[…]` markers intact. The log view parses those into collapsible, coloured
/// sections (see `components::log_view`); keeping them here would throw away the
/// very structure it renders.
///
/// The endpoint 302-redirects to a signed URL; `follow_location_to_data` chases
/// it — the same path octocrab's own log-zip download uses, so the redirect and
/// auth are handled for us. Logs exist only for a **completed** job; the caller
/// gates on that (a running job would 404 here).
pub async fn fetch_job_log(
    octocrab: &Octocrab,
    owner: &str,
    repo: &str,
    job_id: u64,
) -> Result<String, String> {
    let uri = format!("/repos/{owner}/{repo}/actions/jobs/{job_id}/logs");
    let response = octocrab
        ._get(uri)
        .await
        .map_err(|err| diagnose(err).message().to_owned())?;
    let response = octocrab
        .follow_location_to_data(response)
        .await
        .map_err(|err| diagnose(err).message().to_owned())?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!(
            "GitHub returned HTTP {} for this job's log",
            status.as_u16()
        ));
    }
    octocrab
        .body_to_string(response)
        .await
        .map_err(|err| diagnose(err).message().to_owned())
}

/// Gather a run's annotations (errors / warnings / notices) from its jobs' check
/// runs — the same set GitHub shows in a run's "Annotations" section. Each job
/// carries a check-run id; we list that check run's annotations via the Checks
/// API and map them into our own type (rule 4). Only **completed** jobs are
/// queried (in-progress ones don't have annotations yet, and it keeps the call
/// count down); a per-job failure is logged and skipped, not fatal. Failures
/// sort first.
pub async fn fetch_annotations(
    octocrab: &Octocrab,
    owner: &str,
    repo: &str,
    jobs: &[Job],
) -> Vec<Annotation> {
    let mut out = Vec::new();
    for job in jobs {
        if job.status != RunStatus::Completed {
            continue;
        }
        let Some(check_run_id) = job.check_run_id else {
            continue;
        };
        match octocrab
            .checks(owner, repo)
            .list_annotations(check_run_id.into())
            .send()
            .await
        {
            Ok(annotations) => {
                for annotation in annotations {
                    out.push(Annotation {
                        level: AnnotationLevel::from_api(annotation.annotation_level.as_deref()),
                        title: annotation.title.filter(|title| !title.is_empty()),
                        message: annotation.message.unwrap_or_default(),
                        location: annotation_location(&annotation.path, annotation.start_line),
                        job: job.name.clone(),
                    });
                }
            }
            Err(err) => {
                tracing::debug!(job = %job.name, "annotations fetch failed: {}", diagnose(err).message());
            }
        }
    }
    out.sort_by_key(|annotation| annotation.level.order());
    out
}

/// `path:line` for a file-bound annotation; empty for run-level ones (GitHub uses
/// `.github` as the path for those).
fn annotation_location(path: &str, line: u32) -> String {
    if path.is_empty() || path == ".github" {
        String::new()
    } else {
        format!("{path}:{line}")
    }
}

/// Re-run **all** of a run's jobs. octocrab 0.54 has no helper for this endpoint
/// (only `cancel_workflow_run`), so it's a raw `POST` — verified against the
/// REST docs, not memory (rule 1). GitHub answers `201 Created` on success.
pub async fn rerun_run(
    octocrab: &Octocrab,
    owner: &str,
    repo: &str,
    run_id: u64,
) -> Result<(), String> {
    post_action(
        octocrab,
        format!("/repos/{owner}/{repo}/actions/runs/{run_id}/rerun"),
    )
    .await
}

/// Re-run only a run's **failed** jobs (and anything downstream of them). Same
/// raw-`POST` shape as [`rerun_run`], different endpoint.
pub async fn rerun_failed_jobs(
    octocrab: &Octocrab,
    owner: &str,
    repo: &str,
    run_id: u64,
) -> Result<(), String> {
    post_action(
        octocrab,
        format!("/repos/{owner}/{repo}/actions/runs/{run_id}/rerun-failed-jobs"),
    )
    .await
}

/// Cancel an in-progress run. This one *does* have an octocrab helper.
pub async fn cancel_run(
    octocrab: &Octocrab,
    owner: &str,
    repo: &str,
    run_id: u64,
) -> Result<(), String> {
    octocrab
        .actions()
        .cancel_workflow_run(owner, repo, run_id.into())
        .await
        .map_err(|err| diagnose(err).message().to_owned())
}

/// `POST` an action endpoint that takes no body and returns no data. `_post`
/// yields the raw response (it doesn't error on a 4xx), so we check the status
/// and name the fix ourselves — the same contract the read paths use (rule 2).
async fn post_action(octocrab: &Octocrab, uri: String) -> Result<(), String> {
    let response = octocrab
        ._post(uri, None::<&()>)
        .await
        .map_err(|err| diagnose(err).message().to_owned())?;
    let status = response.status();
    if status.is_success() {
        Ok(())
    } else {
        Err(action_error(status))
    }
}

/// A run-action failure message that names the fix (rule 2).
fn action_error(status: http::StatusCode) -> String {
    match status.as_u16() {
        401 => "Sign in again — GitHub rejected the token.".to_owned(),
        403 => "Not allowed — the token may lack permission, or you're rate-limited.".to_owned(),
        404 => "That run is gone, or you don't have access to it.".to_owned(),
        409 => "The run isn't in a state that allows that action.".to_owned(),
        code => format!("GitHub returned HTTP {code}."),
    }
}

/// Step 1 of the device flow: ask GitHub for a user code. The returned
/// `DeviceFlow` carries the code to show the user and the handle to poll with.
///
/// The device endpoints live on **github.com** (not api.github.com) and default
/// to a form-encoded response, so this client is built pointing there with an
/// `ACCEPT: application/json` header — both requirements come straight from
/// octocrab's docs for `authenticate_as_device`.
pub async fn start_device_flow() -> Result<DeviceFlow, ConnectError> {
    let crab = Octocrab::builder()
        .base_uri("https://github.com")
        .map_err(diagnose)?
        .add_header(ACCEPT, "application/json".to_owned())
        .build()
        .map_err(diagnose)?;

    let client_id = SecretString::from(CLIENT_ID.to_owned());
    let codes = crab
        .authenticate_as_device(&client_id, SCOPES)
        .await
        .map_err(diagnose)?;

    Ok(DeviceFlow {
        user_code: codes.user_code.clone(),
        verification_uri: codes.verification_uri.clone(),
        crab,
        codes,
    })
}

/// An in-progress device-flow authorisation. Holds octocrab's `DeviceCodes` and
/// the client to poll with — both opaque to the UI (CLAUDE.md rule 4), which
/// only reads `user_code` / `verification_uri` to display.
pub struct DeviceFlow {
    pub user_code: String,
    pub verification_uri: String,
    crab: Octocrab,
    codes: octocrab::auth::DeviceCodes,
}

impl DeviceFlow {
    /// Step 2: poll until the user authorises (or the code expires), then return
    /// the OAuth access token. octocrab owns the poll interval and GitHub's
    /// `slow_down` backoff internally; this blocks, so it runs off the GTK main
    /// thread inside a relm4 command.
    pub async fn poll(self) -> Result<String, ConnectError> {
        let client_id = SecretString::from(CLIENT_ID.to_owned());
        let oauth = self
            .codes
            .poll_until_available(&self.crab, &client_id)
            .await
            .map_err(diagnose)?;
        // Pull the token out of its `SecretString` just long enough to hand it
        // to the keyring — it is never logged.
        Ok(oauth.access_token.expose_secret().to_owned())
    }
}

/// Turn an octocrab error into a `ConnectError` whose message names the fix.
fn diagnose(err: octocrab::Error) -> ConnectError {
    match &err {
        // A response came back carrying an HTTP error status.
        octocrab::Error::GitHub { source, .. } => match source.status_code.as_u16() {
            401 => ConnectError::Auth(
                "GitHub rejected the token (401). It may be invalid or expired — \
                 paste a new fine-grained token."
                    .to_owned(),
            ),
            403 => ConnectError::RateLimited(
                "GitHub returned 403 — the rate limit is reached, or the token \
                 doesn't have the access it needs."
                    .to_owned(),
            ),
            404 => ConnectError::Other(
                "Not found (404) — the token may not be able to see that resource.".to_owned(),
            ),
            code => ConnectError::Other(format!("GitHub error {code}: {}", source.message)),
        },
        // No response at all: the transport failed. Keep it short and actionable
        // rather than dumping a URL-bearing chain.
        octocrab::Error::Hyper { .. } | octocrab::Error::Service { .. } => {
            ConnectError::Offline("Can't reach GitHub — check your internet connection.".to_owned())
        }
        other => ConnectError::Other(format!("Unexpected error: {other}")),
    }
}
