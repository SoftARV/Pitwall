# Pitwall — how it works

A native GNOME app to monitor GitHub Actions across a hand-picked set of repos,
on one Linux laptop.

This document is the map: what the pieces are, why they're shaped that way, and
what's built versus what isn't. `CLAUDE.md` is the *rulebook* (what we may and
may not do); this is the *explanation*. It's the sibling of **Dockyard** and
borrows its architecture wholesale — where a pattern is identical, this points at
Dockyard rather than repeating it.

Like Dockyard, it assumes strong TypeScript/React and no Rust, and leans on that
comparison where it genuinely helps.

---

## The big picture: Redux, with a compiler

relm4 *is* the Elm architecture, which is where Redux came from. Same mapping as
Dockyard:

| Redux / React        | relm4                | Here                       |
| -------------------- | -------------------- | -------------------------- |
| store state          | the model struct     | `AppModel` (`src/app.rs`)  |
| action               | the `Input` enum     | `AppMsg`                   |
| reducer              | `update()`           | `AppModel::update`         |
| `useSelector` + JSX  | the `view!` macro    | `AppModel`'s `view!`       |
| thunk / saga         | a `Command`          | `sender.oneshot_command`   |
| child `onChange`     | the `Output` enum    | `RunRowOutput`             |

The view is built once and mutated via `#[watch]`, not re-rendered; `update()`
must not await (it runs on the GTK main thread); messages are typed and
exhaustive. All of that is explained at length in Dockyard's ARCHITECTURE.md and
holds identically here.

---

## What makes Pitwall different from Dockyard

One fact drives every non-trivial decision: **the data source is a remote,
authenticated, rate-limited HTTP API, not a local socket.** The consequences:

### Auth is a first-class state

Dockyard is either connected or it isn't. Pitwall has a third state *before*
that: **not signed in**. The `ViewState` is `Loading | SignedOut | Ready |
Disconnected(reason)`, and `SignedOut` is a *blocking onboarding dialog* over a
neutral backdrop — you sign in or you quit.

Sign-in is GitHub's OAuth **device flow**, not a pasted token: the app shows an
8-character user code, opens `github.com/login/device`, and polls until you
authorise (one streaming relm4 command, so closing the app cancels it). The
resulting token is a **secret**, stored in the GNOME Keyring via `oo7`
(`secret.rs`), never in the config file. A stored token can be revoked, so
"connect" means *build the client and make one real authenticated call* — the
`ping()` analog — before we believe it. See CLAUDE.md rule 2.

### The rate limit is a budget, and ETags are how you afford it

5000 requests/hour. Watching N repos at a fixed interval spends N requests a
poll — which is why **conditional requests** matter: an `If-None-Match` that
returns `304 Not Modified` is *free*. The client caches an ETag per (repo,
endpoint); idle repos cost nothing to re-check. We also read
`X-RateLimit-Remaining` / `-Reset` and back off. This is the single biggest
mechanical difference from Dockyard's "poll every 2s, it's basically free".

### Logs are fetched, not followed — and per **job**, not per step

Docker gives a live `logs --follow` stream that Dockyard cancels on nav-back via
`drop_on_shutdown`. The GitHub REST API has **no equivalent** — job logs are a
download (a redirect to plain text), and only once the job has **completed**. So
Pitwall's log page reuses Dockyard's fixed-dark terminal *look* but is a one-shot
fetch, with an "Open in browser" escape hatch for a still-running job.

Per-*step* logs were tried and abandoned: the raw log has **no step delimiter**
(the `##[group]` markers are sub-sections *within* a step, and runner steps like
"Set up job" have no group of their own), and the jobs API's step timestamps are
only second-precise — so carving one step out is guesswork either way. Instead
the whole job is rendered faithfully: `##[group]` becomes a **collapsible
section** (auto-expanded when it holds an `##[error]`), and the **ANSI colour**
the tools emit is parsed into `gtk::TextTag`s rather than stripped. Parsing runs
off-thread; only widget-building touches the main thread.

### Notifications are the reason it exists

Dockyard never needed to tell you anything — you were looking at it. Pitwall's
job is to tell you a run failed while you were doing something else. On each poll
we diff what we last recorded per run; a freshly-finished run whose conclusion
matches your preference fires a `gio::Notification`
(`Application::send_notification`) — native, app-icon, clickable straight to the
run via an app-scoped `app.open-run` action.

The mechanism is `last_finished: HashMap<RunId, (Conclusion, DateTime<Utc>)>`.
The timestamp matters: **a re-run keeps the same run id**, so presence alone
would miss it — comparing `updated_at` is what catches a re-run that finishes
between two polls. The first poll after connecting seeds the baseline *silently*,
so pre-existing failures don't all alert at startup.

One hard-won environmental caveat: GNOME's notification backend **drops
notifications from an app it can't resolve to an installed `.desktop` entry with
a matching icon**. The feature therefore depends on `make install` (and, the
first time, a re-login so GNOME Shell reloads the icon theme) — not on anything
in the code.

### The lifecycle actions map cleanly — but need optimism

Docker's start / stop / restart / remove become **re-run / re-run-failed /
cancel**. The shape is identical: the row emits intent, the root reducer decides,
an off-thread `oneshot_command` does the HTTP, it refreshes on completion,
failures become toasts. Anything we don't do natively (view a diff, edit a
workflow) is an **"Open in browser"** on `html_url` rather than scope creep.

What's *not* the same is the feedback delay. Docker's state changes are instant;
GitHub's list API lags its own `201`, so a naive "poll after action" leaves the
row showing the old state for seconds. So the reducer shows the knowable next
state immediately:

- **re-run** flips the run to `Queued` at once — safe against a stale poll
  overwriting it, because the ETag makes an unchanged list answer `304`, and a
  `200` that *does* arrive already carries the new state;
- **cancel** overlays a client-side `RunStatus::Cancelling` — a status GitHub
  never reports (the run stays `in_progress` until it has finished stopping) —
  held until a poll shows the run completed, with the action button disabled
  meanwhile;
- both then fire a short **follow-up poll burst** (~3s / ~9s) so the real state
  lands in seconds rather than at the next full interval.

### The main view is two tabs, and search takes over

Dockyard has one list. Pitwall has an `adw::ViewStack` behind a header
`ViewSwitcher`: **Recent** (the newest `RECENT_CAP` runs across the watchlist,
with a failing-count badge on the tab) and **Repositories** (one expander per
watched repo showing its latest run, expanding to that repo's history). Starting
a search *hides the tabs* and shows one flat list of all matches across the
watchlist — a dedicated results screen — because a filter that only searched the
tab you happened to be on would be a trap.

The repos view and the detail page's jobs follow the same imperative-rebuild
discipline: rebuild **only when the data actually changed**, and restore which
rows were expanded, so a background poll never collapses what you were reading
(rule 7, learned the hard way in Dockyard).

---

## The type boundary (`github/types.rs`)

octocrab's models mirror the GitHub JSON — lots of `Option`, enums as strings,
fields we don't care about. As in Dockyard, we resolve all of that **once** at
the boundary into plain owned structs the UI is pleasant to write against:

```
Repo         owner/name, private
RateLimit    remaining, limit, reset  (+ resets_in_minutes / is_exhausted)
WorkflowRun  id, run #, workflow name, repo, branch, event,
             RunStatus (queued|in_progress|completed|cancelling|unknown),
             Conclusion (success|failure|cancelled|skipped|timed_out|…),
             commit sha+message+author, created/updated, html_url
Job          id, name, RunStatus, Conclusion, started/completed, steps[],
             check_run_id  (parsed from check_run_url — the annotations key)
Step         name, RunStatus, Conclusion, started/completed
Annotation   AnnotationLevel (failure|warning|notice), title, message,
             location (path:line), job
```

`RunStatus`/`Conclusion` drive the shared `StatusChip` the way `ContainerState`
did in Dockyard. Two wrinkles worth noting: `RunStatus::Cancelling` is
**client-only** (never produced by the API mapping — see the actions section
above), and the ETag cache lives in the root model keyed by repo, not on `Repo`
itself. This module owns the mapping and the unit tests; `octocrab` never appears
outside it (CLAUDE.md rule 4).

**Annotations** come from a different API than everything else: the warnings /
errors / notices GitHub shows under a run are *check-run* data, so they're
fetched per completed job via `checks().list_annotations(check_run_id)` — which
is why `Job` carries the id parsed out of its `check_run_url`.

---

## Decisions worth remembering

| Decision | Why |
| --- | --- |
| Reuse Dockyard's stack (relm4/gtk4/libadwaita) | Proven native-GNOME feel; the author knows it; the architecture transfers ~wholesale. |
| `octocrab` for the API | The mature Rust GitHub client, with an Actions surface (workflows/runs/jobs/logs/rerun/cancel). The bollard analog — and CLAUDE.md rule 1 (don't trust training data for it) is inherited. |
| Token in the keyring via `oo7` | It's a credential, not a preference. `oo7` is pure-Rust Secret Service by GNOME devs; 0.6 is the stable line (0.7 is alpha). A plaintext config token was considered and rejected. |
| Hand-picked watch list | Matches "the repos I care about" and is cheapest on the rate limit; there's no cross-repo "all my runs" REST endpoint anyway. Auto-watch-everything is heavier and noisier. |
| Poll 45s + ETag, not 2s | Remote + rate-limited. Conditional requests make idle repos free; the suspend-gate still applies (network + battery). Adaptive (fast while running) is a later win, per CLAUDE.md. |
| Logs fetched, not streamed | REST has no live log follow; logs download only for completed jobs. An honest limit, surfaced with "Open in browser" for running jobs. |
| Device flow, not a pasted PAT | Nicer UX for a desktop app (no token screen, no scope-picking), and the client id is public by design. The `repo` scope is broad, but it's the narrowest that reads Actions on private repos — acceptable for a personal monitor of your own repos. |
| Per-**job** log endpoint, not the run zip | The run-log zip holds only per-job files (no per-step ones), so it's a bigger download for the same data. Per-step splitting was tried against both `##[group]` markers and step timestamps and abandoned — the raw log simply doesn't delimit steps. |
| Optimistic re-run / client-side `Cancelling` | GitHub's list API lags its own 201/202. The ETag makes optimism safe (a stale list 304s), and cancel would otherwise read "Running" for many seconds while GitHub stops the jobs. |
| Annotations via the Checks API | It's where GitHub actually keeps them; the Actions API doesn't expose them. Fetched only for completed jobs, and only when the jobs change, to stay cheap. |
| Notifications early | It's the product's reason to exist; the transition-diff is trivial and load-bearing. Keyed on `(conclusion, updated_at)` because re-runs reuse the run id. |
| Icon + `.desktop` are a *feature* dependency | GNOME drops notifications from an app it can't resolve, so packaging isn't cosmetic here — it's what makes the headline feature work. |
| App ID `dev.miguelrincon.Pitwall` | Matches the `.desktop` name, the GResource prefix, and `RelmApp::new()`. The join key GNOME uses to attach a window to its launcher/icon. |
| No Flatpak in v1 | Same as Dockyard for now — though a Flatpak Pitwall is more plausible (needs only network, not a socket). Later. |

---

## Status

Planned and built 24–25 Jul 2026; **v0.1.0 released**. All seven v1 milestones
shipped, one PR each:

| Milestone | What landed |
| --- | --- |
| M1 | Scaffold, OAuth device-flow sign-in + keyring, repo picker, run list, suspend-gated poll |
| M2 | Conditional (ETag) polling, rate-limit awareness + backoff banner |
| M3 | Run detail: jobs, steps, metadata |
| M4 | Job logs — collapsible sections, ANSI colour, fixed-dark terminal |
| M5 | Re-run / re-run-failed / cancel, optimistic feedback, live detail page |
| M6 | Desktop notifications on completion (configurable) |
| M7 | Icon + `.desktop` + installer, Preferences, search, Recent/Repositories tabs, annotations, shortcuts |

### Where the code lives

```
src/
  main.rs        bootstrap: tracing, icon, settings + theme, per-component CSS
  app.rs         the root component — model, AppMsg, update, view; tabs, search,
                 poll, notifications transition-diff, run actions, Preferences
  settings.rs    ~/.config/pitwall/settings.ini (theme, watchlist, notify, interval)
  secret.rs      oo7 keyring wrapper — the token, and only the token
  notify.rs      gio::Notification construction + send
  github/
    client.rs    octocrab: connect+verify, conditional poll, jobs, logs,
                 annotations, re-run/cancel, error diagnosis
    types.rs     our types — octocrab stops here (+ unit tests)
  components/
    run_row.rs      FactoryComponent → the run list row (+ shared action spec)
    run_detail.rs   the run page: metadata, annotations, jobs → steps
    log_view.rs     a job's log page: collapsible, ANSI-coloured, fixed dark
    repo_picker.rs  the watch-list dialog
    status_chip.rs  the shared status pill
```

### Not built (deliberately)

Adaptive polling (faster while a run is active) is still the "later win" CLAUDE.md
describes — the fixed interval is now user-configurable instead. No Flatpak yet,
no workflow editing, secrets management, `workflow_dispatch` inputs, or
multi-account: all out of scope for a personal, single-machine monitor.
