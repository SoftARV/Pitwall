// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! The GitHub client: build, verify, and diagnose.
//!
//! CLAUDE.md rule 1: octocrab's API shifts between versions, so every call here
//! was checked against octocrab 0.54's own source, not memory. Rule 4: octocrab
//! types are mapped into ours (`RateLimit`) before leaving this module.

use http::header::ACCEPT;
use octocrab::Octocrab;
use secrecy::{ExposeSecret, SecretString};

use crate::github::types::{RateLimit, Repo, WorkflowRun};

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

/// List recent workflow runs across the watched repos, newest first.
///
/// One request per repo (ETag conditional requests will make idle ones free —
/// that's M2). A single repo failing — deleted, renamed, access revoked — is
/// logged and skipped rather than sinking the whole poll; but if *every* repo
/// fails (the offline / bad-token case) that's surfaced, so the UI never renders
/// an empty, healthy-looking list against a dead connection (CLAUDE.md rule 2).
pub async fn list_runs(octocrab: &Octocrab, repos: &[String]) -> Result<Vec<WorkflowRun>, String> {
    let mut runs = Vec::new();
    let mut failures = 0;
    let mut last_error = String::new();

    for repo in repos {
        let Some((owner, name)) = repo.split_once('/') else {
            continue;
        };
        match octocrab
            .workflows(owner, name)
            .list_all_runs()
            .per_page(20)
            .send()
            .await
        {
            Ok(page) => runs.extend(
                page.items
                    .into_iter()
                    .map(|run| WorkflowRun::from_model(run, repo.clone())),
            ),
            Err(err) => {
                failures += 1;
                last_error = diagnose(err).message().to_owned();
                tracing::warn!(repo = %repo, "couldn't list runs: {last_error}");
            }
        }
    }

    if !repos.is_empty() && failures == repos.len() {
        return Err(last_error);
    }

    // Newest first: the list reads top-down like an activity feed.
    runs.sort_by_key(|run| std::cmp::Reverse(run.created_at));
    Ok(runs)
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
