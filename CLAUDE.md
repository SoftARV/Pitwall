# CLAUDE.md

Project instructions for Claude Code. Read this fully before writing code.

## What this is

**Pitwall** — a small, native GNOME desktop app to *monitor* GitHub Actions
across a hand-picked set of your repositories, from one personal Linux laptop.
Not a product, not multi-user, not cross-platform. One user, one machine.

It is the sibling of **Dockyard** (a native GNOME Docker manager) and shares its
stack, its architecture, and its taste. The name comes from a motorsport pit
wall: the place the team sits watching live timing as the race unfolds and
calls strategy. That is the app — watch runs go green/red, and jump in to
re-run or cancel.

The app should be indistinguishable from a first-party GNOME application. If a
design decision would make it look like an Electron app or a generic Qt tool, it
is the wrong decision.

**Dockyard is a *manager*; Pitwall is a *monitor*.** The headline feature is a
native desktop **notification** when a watched run fails while the window is in
the background. Build the rest in service of that.

## Author context — read this, it changes how you should respond

The author is a senior frontend engineer (~10 years: TypeScript, React, React
Native, Node) who is **new to Rust**. Consequences:

- When you introduce ownership, borrowing, lifetimes, `Rc`/`Arc`/`RefCell`, or
  `async` pinning, **briefly explain why** in a comment or in your reply. Do not
  silently sprinkle `.clone()` to quiet the borrow checker — say what the
  ownership problem was and why the clone is the right or pragmatic fix.
- Analogies to React/Redux land well. relm4 *is* the Elm architecture; say so.
- Do not dumb down the Rust. Idiomatic code with explanation, not beginner code.
- Prefer clarity over cleverness. No macro tricks, no premature generics.

## The one axis that differs from Dockyard

Dockyard talks to a **local Unix socket** — free, instant, no auth. Pitwall talks
to a **remote, authenticated, rate-limited HTTP API**. Almost every hard rule
below is a consequence of that single difference. Keep it in mind:

| Concern     | Dockyard (local Docker)      | Pitwall (remote GitHub)                     |
| ----------- | ---------------------------- | ------------------------------------------- |
| Transport   | Unix socket, ~1ms, free      | HTTPS; latency; can be offline              |
| Auth        | Being in the `docker` group  | A **token** you must store securely         |
| Poll cost   | 2s poll, ~0.085% of a core   | Rate-limited (5000/hr); 30–60s + conditional |
| Logs        | True live `--follow` stream  | **No live stream** — download, completed jobs only |
| Killer feat | (managing)                   | **Desktop notifications** on failure        |

## Stack (pinned — do not swap these out)

| Layer          | Crate                    | Version |
| -------------- | ------------------------ | ------- |
| UI framework   | `relm4`                  | 0.11 (features: `libadwaita`, `gnome_49`) |
| Widgets        | `gtk4`, `libadwaita`     | via relm4 (do **not** add directly) |
| GitHub client  | `octocrab`               | 0.54    |
| Secret storage | `oo7`                    | 0.6 (Secret Service / GNOME Keyring) |
| Async runtime  | `tokio`                  | 1       |
| Streams        | `futures-util`           | 0.3     |
| Timestamps     | `chrono`                 | 0.4     |
| Logging        | `tracing`                | 0.1     |

Rust edition 2024. Plus `anyhow` 1 (rule 6), `tracing-subscriber` 0.3 with
`env-filter` for `RUST_LOG`, and `http` 1 / `secrecy` 0.10 for the device-flow
sign-in (octocrab's `authenticate_as_device` wants a `SecretString` client id and
an `ACCEPT` header; both crates are already in octocrab's tree, so they're
near-free direct adds). Toolchain must be ≥ 1.93 (relm4 0.11's MSRV);
libadwaita ≥ 1.8 / GTK ≥ 4.20 (the `gnome_49` floor — same reasoning as
Dockyard: `adw::ShortcutsDialog` and the modern dialogs).

**relm4 0.11's docs.rs build is broken.** Read the vendored source, which is the
exact version we compile against:

```bash
ls ~/.cargo/registry/src/*/relm4-0.11.0/src/
```

**relm4, not raw gtk4-rs.** Every component is a relm4 `Component` or
`FactoryComponent`. Reaching for `Rc<RefCell<>>` to share widget state is a sign
the state belongs in a model and the change belongs in an `update()`.

## Hard rules

### 1. Never trust your training data for the octocrab API

octocrab's handler/builder API has changed across versions, and most examples
online are for older ones. **Before writing any octocrab call you haven't
already written in this repo, check https://docs.rs/octocrab/0.54.** The Actions
surface we use lives under handlers like:

```rust
// Verify each signature against docs.rs/0.54 before relying on it.
let runs = octocrab
    .workflows(owner, repo)
    .list_all_runs()        // or .list_runs(workflow) for one workflow
    .per_page(30)
    .send()
    .await?;
```

Jobs, logs, re-run and cancel live under the actions handler
(`octocrab.actions()`). Map response models to our own types (rule 4).

### 2. Auth is GitHub's OAuth **device flow**; the token is a secret

Sign-in uses the OAuth **device flow** — the flow built for native apps: no
client secret, only a **public** `client_id` embedded in the source
(`client::CLIENT_ID`; committing it is fine — every device-flow client ships one,
`gh`'s included). First launch shows a "Sign in with GitHub" button; Pitwall then
shows an 8-character user code, opens `github.com/login/device` in the browser
and copies the code, and polls until you authorise. octocrab drives it
(`authenticate_as_device` → `poll_until_available`), inside **one streaming relm4
command** so it's cancelled if the app closes mid-flow. Scope is **`repo`** —
GitHub has no Actions-only OAuth scope, so it's the narrowest that reads Actions
on private + public repos (and later re-runs/cancels). Device-flow-*only* was a
deliberate choice over a PAT paste: nicer UX, and the broad scope is acceptable
for a personal monitor of your own repos.

The resulting OAuth access token is stored in the **Secret Service (GNOME
Keyring) via `oo7`**, keyed by the app ID. Rules:

- **Never** write the token to the config file, to `tracing`, or to any error
  string. `settings.rs` persists *preferences*; the token is not one. The
  `client_id` is public and is **not** the secret — only the access token is.
- **Verify, don't assume.** A stored token can be revoked or expired. After
  building the client, make one cheap authenticated call (`/user` + `/rate_limit`)
  — the octocrab analog of Dockyard's `ping()` — before declaring "connected". A
  present-but-dead token must not render an empty, healthy-looking list.
- Errors name the fix: **401** → "sign in again"; **403 rate limit** → "rate
  limit reached, resets in N min"; **404** → "repo not found or no access";
  network → "offline".

### 3. Rate limits are the budget — poll conditionally

Authenticated REST is **5000 requests/hour**. Monitoring N repos naively costs N
requests per poll. The rule that makes this cheap:

- **Conditional requests.** Store an **ETag** per (repo, endpoint); send
  `If-None-Match`. A **`304 Not Modified` does not count against the rate
  limit** — idle repos become free to poll. octocrab's built-in ETag handling is
  thin, so this may require a lower-level request with custom headers; verify.
- **Respect the headers.** Read `X-RateLimit-Remaining` / `X-RateLimit-Reset`
  and back off as you approach zero; honor `Retry-After` and `X-Poll-Interval`
  where present. Never hammer.
- **Pause when hidden.** The poll timer is removed entirely while the window
  isn't visible (`gtk::Window::is_suspended`) — same as Dockyard, and it matters
  more here: it's network and battery, not just wakeups.

### 4. GitHub types must not leak into the UI

Map octocrab's response models into our own `WorkflowRun` / `Job` / `Step` /
`Repo` structs in `github/types.rs` at the boundary — "parse, don't validate".
The `view!` macro and the components only ever see our types, never octocrab's.

### 5. Never block the GTK main thread

All HTTP goes through relm4 `Command`s (`oneshot_command`; `command` only if a
genuine stream appears). `update()` stays synchronous and fast. A slow request
must never stall a window drag.

### 6. No `.unwrap()` / `.expect()` outside `main.rs` and tests

Network calls fail routinely — offline, 401, 403, a repo deleted between polls.
Every failure becomes an `adw::Toast` (or a state), never a crash. Use
`anyhow::Result` internally.

### 7. Update rows in place; the poll is silent

Reconcile the run list by run id — rebuild widgets only when membership changes
(Dockyard learned this the hard way: rebuilding tears down open popovers and
hides staleness bugs). The background poll shows no spinner; only a
user-initiated refresh does.

## Architecture

```
src/
  main.rs              # RelmApp bootstrap, tracing, icon; load settings + apply theme;
                       #   install each component's CSS
  app.rs               # root Component: AppModel, AppMsg, update, view; search/filter,
                       #   Preferences, the repo picker entry point
  settings.rs          # persistent PREFERENCES via glib::KeyFile
                       #   (~/.config/pitwall/settings.ini): watched repos, poll
                       #   interval, notification prefs, theme. NEVER the token.
  secret.rs            # oo7 wrapper: store / load / clear the GitHub token
  notify.rs            # gio::Notification helpers (failure alerts)
  github/
    mod.rs
    client.rs          # octocrab client build + verify; thin async wrappers
                       #   (list runs, jobs, logs, re-run, cancel); ETag cache;
                       #   rate-limit awareness; error diagnosis that names the fix
    types.rs           # our WorkflowRun / Job / Step / Repo / RunStatus /
                       #   Conclusion / Annotation / RateLimit (+ tests).
                       #   octocrab stops here.
  components/
    mod.rs
    run_row.rs         # FactoryComponent -> adw::ActionRow (a workflow run)
    run_detail.rs      # Component -> detail page (NavigationPage): run metadata,
                       #   annotations, jobs + steps; pushes the log page. Stays
                       #   live while open (the app forwards each poll to it).
    log_view.rs        # Component -> a job's log page (NavigationPage); fetched
                       #   (not streamed), whole-job, with collapsible ##[group]
                       #   sections and ANSI colour, in a fixed-dark "terminal"
    status_chip.rs     # shared WidgetTemplate (pill + dot); Conclusion/RunStatus
                       #   -> label/variant. Reused from Dockyard's design.
    repo_picker.rs     # Component/dialog -> choose the watched repo set
data/
  dev.miguelrincon.Pitwall.desktop
  icons/hicolor/{16x16,...,512x512,scalable}/apps/dev.miguelrincon.Pitwall.{png,svg}
  screenshots/           # README gallery (list.png, detail.png)
Makefile               # make install -> ~/.local (no sudo); make uninstall; make check
```

Dependency direction is strictly one-way, as in Dockyard:
`main -> app -> components/*`, and `app -> github/client -> github/types ->
[octocrab]`; `components/` never imports `octocrab`.

The root model is roughly (as built — see `src/app.rs` for the full set):

```rust
struct AppModel {
    connection: Option<Connection>,      // built octocrab client + verified user/limits
    watched: Vec<Repo>,                  // the hand-picked set (from settings)
    runs: FactoryVecDeque<RunRow>,       // the *visible* (Recent, or search results) runs
    all_runs: Vec<WorkflowRun>,          // full set from the last poll; the filter reads this
    last_finished: HashMap<RunId, (Conclusion, DateTime<Utc>)>, // transitions -> notify
                                         // (the timestamp catches re-runs: same id, later finish)
    cancelling: HashSet<RunId>,          // client-side "Cancelling" overlay until GitHub finishes
    query: String,                       // search text
    state: ViewState,                    // Loading | SignedOut | Ready | Disconnected(String)
    pending: HashMap<RunId, RunAction>,  // re-run / cancel in flight
    refreshing: bool,                    // user-initiated refresh only
    poll: Option<glib::SourceId>,        // None while the window is hidden
    rate: Option<RateLimit>,             // remaining / reset, for the UI + backoff
    settings: Settings,
    toast_overlay: adw::ToastOverlay,
    // plus nav / detail / detail_id / action handles for imperative use
}

enum AppMsg {
    Refresh, ManualRefresh,
    SignIn, CancelAuth, SignOut,         // OAuth device-flow sign-in
    Rerun(RunId), RerunFailed(RunId), Cancel(RunId),
    ShowDetails(RunId),
    OpenInBrowser(RunId),                // html_url -> the web UI, for anything we don't do natively
    SearchChanged(String),
    EditWatchlist,
    ShowAbout, ShowPreferences, ShowShortcuts,
    SetPollInterval(u32), SetNotifyOn(NotifyOn), SetTheme(Theme),
    Error(String),
    SuspendedChanged(bool),
}

// Off-thread results (relm4's CommandOutput channel — distinct from AppMsg).
enum CommandMsg {
    Connected(Box<Result<Connection, String>>),
    RunsLoaded { runs: Vec<WorkflowRun>, rate: Option<RateLimit> },
    JobsLoaded(RunId, Vec<Job>),
    LogLoaded(JobId, Result<String, String>),
    ActionDone { id: RunId, action: RunAction, result: Result<(), String> },
    LoadFailed(String),
}
```

This is Redux with a compiler: actions in, one reducer, view derived from state.

## UI shape

- `adw::ApplicationWindow` > `adw::ToolbarView` > `adw::HeaderBar`, opening wide.
- Header title = an `adw::ViewSwitcher` over the **Recent / Repositories** tabs
  once there are runs (a plain `adw::WindowTitle` otherwise); the failing count
  rides as a **badge on the Recent tab**. Left: a **search** toggle +
  `gtk::SearchBar` (Ctrl+F, or type-to-search) filtering by repo / workflow /
  branch, client-side. Right: the **primary menu** (hamburger) — Refresh
  (Ctrl+R/F5), Edit Watchlist, Preferences (Ctrl+,), Keyboard Shortcuts (Ctrl+?),
  About, Quit (Ctrl+Q), each a `GAction` posting an `AppMsg`.
- Main content: `adw::NavigationView`. Root = an `adw::ViewStack` (clamped,
  ~700px) with **Recent** (newest `RECENT_CAP` runs) and **Repositories** (one
  expander per watched repo → its latest run, expanding to that repo's history).
  A search **takes over the surface**: the tabs hide and one flat list shows all
  matches across the watchlist. Clicking a run pushes the **detail** page, and a
  job's "View log" pushes the **log** page on top of that.
- Each run is an `adw::ActionRow`: title = workflow name (+ run #), subtitle =
  `repo · branch · event · relative time`, a **status chip** prefix (from
  `Conclusion`/`RunStatus`), and single-click action buttons — the primary one
  (re-run when finished / cancel while active, disabled while cancelling) plus
  open-in-browser. Activating the row opens the detail page.
- **Status chip** = the shared `StatusChip` `WidgetTemplate` (pill + coloured
  dot): success=green, failure=red, in-progress=blue (subtle pulse), cancelled=
  neutral, queued=amber, skipped=dim. Same widget on the row and the detail page.
- **Detail** page: run metadata (repo, branch, commit title+sha, author, event,
  elapsed), an **Annotations** group (warnings / errors / notices from the jobs'
  check runs, hidden when empty), and a list of jobs each expandable to its steps
  (status + duration) with a "View log" button. Header carries the same run
  actions. Stays live while open — the app forwards each poll to it.
- **Log page**: fixed-dark terminal look (its own `.log-terminal` CSS, theme-
  independent) — **fetched, not streamed** (REST has no follow; completed jobs
  only) and **whole-job**, because the raw log has no reliable per-step
  delimiter. `##[group]` becomes a collapsible section (auto-expanded if it holds
  an `##[error]`), ANSI colour is parsed into `TextTag`s, and `##[command]` /
  error / warning lines are coloured. Parse off-thread; spinner while fetching;
  "Open in browser" for a running job whose logs aren't downloadable yet.
- **First run / not signed in**: a **blocking onboarding modal** (`adw::Dialog`,
  non-closable) — an app intro + a GitHub-coloured "Sign in with GitHub" button →
  the device-flow user code + browser authorisation; it closes onto the app once
  connected. The **Sign Out** menu item is `hidden-when="action-disabled"`, so it
  shows only while signed in. **No watched repos**: a StatusPage inviting
  "Add repositories" → the repo picker. Empty / no-results / disconnected /
  rate-limited: distinct `adw::StatusPage`s. Errors: `adw::ToastOverlay`.
- **Use libadwaita widgets, not raw GTK.** `adw::ActionRow`, `adw::PreferencesGroup`,
  `adw::AboutDialog`, `adw::PreferencesDialog`. That's where the native feel comes
  from. No custom CSS unless there's no libadwaita widget for the job (the two
  known exceptions, inherited from Dockyard's rationale: the `.status-chip` pill
  and the `.log-terminal` look) — say why before adding any.

## Refresh & notifications

- **Poll.** `glib::timeout` at the configured interval (default 45s; floor ~20s)
  → `AppMsg::Refresh` → for each watched repo, a conditional list-runs request.
  Removed while the window is hidden. Rows updated in place (rule 7).
- **Adaptive is a later win, not v1.** Poll faster while a run is `in_progress`,
  slower when everything's terminal — but only after the fixed-interval version
  is proven. Same posture as Dockyard's phase-1 poll.
- **Notifications.** Keep `last_finished` per run — `(conclusion, updated_at)`,
  **not** just the conclusion, because a **re-run reuses the run id**: only the
  timestamp moves, so presence alone would miss it. When a poll shows a run newly
  finished and its `conclusion` matches the user's setting (failures-only / all /
  off), fire a `gio::Notification` via `Application::send_notification` (native,
  app icon, clickable to open the run via the app-scoped `app.open-run` action).
  The first poll after connecting **seeds the baseline silently**, so old failures
  don't all alert at startup. This is the point of the app — keep it reliable.
- **Notifications need the app installed.** GNOME's notification backend drops
  notifications from an app it can't resolve to an installed `.desktop` + icon.
  Testing them means `make install` (and a re-login the first time, so GNOME Shell
  reloads the icon theme) — a `cargo run` build alone will send and show nothing.

## Scope

**v1 shipped in v0.1.0** (Dockyard's issue-driven cadence — one small vertical
slice, one PR each). All seven milestones are done:

- ✅ **M1** — Scaffold + sign-in (device flow → keyring) + connect/verify + pick a
  watch list + list recent runs across it + poll (suspend-gate) + errors that
  name the fix.
- ✅ **M2** — Rows update in place; ETag-per-repo so idle polls are free;
  rate-limit backoff surfaced in the UI.
- ✅ **M3** — Run detail: jobs + steps, run metadata.
- ✅ **M4** — Logs: a completed **job's** text in the fixed-dark panel, with
  collapsible sections and ANSI colour (per-step proved impossible — see
  ARCHITECTURE.md).
- ✅ **M5** — Actions: re-run / re-run-failed / cancel (off-thread, toast on
  failure, confirm cancel, optimistic feedback); "open in browser" for the rest.
- ✅ **M6** — Desktop notifications on completion (transition detection),
  configurable.
- ✅ **M7** — Polish: search, Preferences (interval, notifications, theme),
  Recent/Repositories tabs, annotations, icon + `.desktop` + installer,
  shortcuts, About.

Post-v1, `main` carries a `-dev` version again. The known "later win" still open
from v1 is **adaptive polling** (faster while a run is active) — the interval is
user-configurable in the meantime.

**Stay lean — flag the drift, don't gatekeep.** Not the default focus: editing
workflow files, secrets/variables management, triggering `workflow_dispatch`
with inputs, PR/issue browsing, multi-account, GitHub Enterprise Server, org-wide
dashboards. The app is a personal, single-machine *monitor*, not a GitHub client.
When a change drifts that way, **name the cost and the direction** so it's a
conscious choice — then build it if it genuinely helps on this one machine.

## Commands

```bash
cargo run                  # dev
RUST_LOG=pitwall=debug cargo run
cargo build --release
cargo test                 # pure unit tests; no network needed
cargo clippy --all-targets -- -D warnings   # the bar, before any commit
cargo fmt
```

System deps (CachyOS / Arch):

```bash
sudo pacman -S --needed base-devel pkgconf rust gtk4 libadwaita librsvg
# A keyring provider must be running for oo7 (gnome-keyring is default on GNOME).
```

## Conventions

- `cargo clippy --all-targets -- -D warnings` is the bar, not `cargo build`.
- Commits: conventional commits (`feat:`, `fix:`, `refactor:`, `chore:`).
- **Licence: GPL-3.0-or-later.** Full text in `COPYING`; declared in `Cargo.toml`.
  Every source file carries the two-line SPDX header (`SPDX-FileCopyrightText` +
  `SPDX-License-Identifier: GPL-3.0-or-later`).
- App ID: `dev.miguelrincon.Pitwall`. It must match the `.desktop` file name, the
  GResource prefix (`/dev/miguelrincon/Pitwall/`), and `RelmApp::new()`. The app
  is called **Pitwall** in the window title and `.desktop` `Name=`.
- Versioning: SemVer in `Cargo.toml`; `main` carries a `-dev` pre-release; tags
  are annotated `vX.Y.Z`. (Same release flow as Dockyard.)
- No Flatpak for now — plain `cargo build --release` + a `.desktop` file. (A
  Flatpak'd Pitwall is actually more plausible than Dockyard's, since it only
  needs network, not a socket — a later consideration, not v1.)

## When you're unsure

Ask before: adding a dependency, introducing a new module, or deviating from the
relm4 component model. Don't ask before: fixing a clippy lint, adding a doc
comment, or checking docs.rs. And **never** guess an octocrab or oo7 signature —
read the docs (rule 1).
