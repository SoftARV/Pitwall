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
that: **no token**. The `ViewState` is `NeedsToken | Loading | Ready |
Disconnected(reason)`. The token is a **secret**, stored in the GNOME Keyring via
`oo7` (`secret.rs`), never in the config file. A stored token can be revoked, so
"connect" means *build the client and make one real authenticated call* — the
`ping()` analog — before we believe it. See CLAUDE.md rule 2.

### The rate limit is a budget, and ETags are how you afford it

5000 requests/hour. Watching N repos at a fixed interval spends N requests a
poll — which is why **conditional requests** matter: an `If-None-Match` that
returns `304 Not Modified` is *free*. The client caches an ETag per (repo,
endpoint); idle repos cost nothing to re-check. We also read
`X-RateLimit-Remaining` / `-Reset` and back off. This is the single biggest
mechanical difference from Dockyard's "poll every 2s, it's basically free".

### Logs are fetched, not followed

Docker gives a live `logs --follow` stream that Dockyard cancels on nav-back via
`drop_on_shutdown`. The GitHub REST API has **no equivalent** — job logs are a
download (a redirect to plain text), and only once the job has **completed**. So
Pitwall's log panel reuses Dockyard's fixed-dark terminal *look* but is a
one-shot fetch, with an "Open in browser" escape hatch for a still-running job.
This is the one place the app is genuinely smaller than its sibling, and it's an
API limit, not a choice.

### Notifications are the reason it exists

Dockyard never needed to tell you anything — you were looking at it. Pitwall's
job is to tell you a run failed while you were doing something else. On each poll
we diff the previous `conclusion` per run; a fresh `completed` + `failure` fires
a `gio::Notification` (`Application::send_notification`) — native, app-icon,
clickable. `last_conclusion: HashMap<RunId, Conclusion>` in the model is the
whole mechanism.

### The lifecycle actions map cleanly

Docker's start / stop / restart / remove become **re-run / re-run-failed /
cancel**. The shape is identical: the row emits intent, the root reducer decides,
an off-thread `oneshot_command` does the HTTP, it refreshes on completion,
failures become toasts. Anything we don't do natively (view a diff, edit a
workflow) is an **"Open in browser"** on `html_url` rather than scope creep.

---

## The type boundary (`github/types.rs`)

octocrab's models mirror the GitHub JSON — lots of `Option`, enums as strings,
fields we don't care about. As in Dockyard, we resolve all of that **once** at
the boundary into plain owned structs the UI is pleasant to write against:

```
Repo         owner/name, private, + the cached ETag for its runs
WorkflowRun  id, run #, workflow name, branch, event, actor,
             RunStatus (queued|in_progress|completed),
             Conclusion (success|failure|cancelled|skipped|timed_out|…),
             commit sha+title, created/updated, html_url
Job          id, name, RunStatus, Conclusion, started/completed, steps[]
Step         name, number, RunStatus, Conclusion
```

`RunStatus`/`Conclusion` drive the shared `StatusChip` the way `ContainerState`
did in Dockyard. This module owns the mapping and the unit tests; `octocrab`
never appears outside it (CLAUDE.md rule 4).

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
| Notifications early | It's the product's reason to exist; the transition-diff is trivial and load-bearing. |
| App ID `dev.miguelrincon.Pitwall` | Matches the `.desktop` name, the GResource prefix, and `RelmApp::new()`. The join key GNOME uses to attach a window to its launcher/icon. |
| No Flatpak in v1 | Same as Dockyard for now — though a Flatpak Pitwall is more plausible (needs only network, not a socket). Later. |

---

## Status and timeline

Planned 24 Jul 2026. The stack, name (**Pitwall**), and the three opening
decisions — a **hand-picked watch list**, the token in the **GNOME keyring**, and
the `octocrab`/`oo7`/relm4 stack — are settled. Scaffolding (build files,
license, these two docs) is in place; no Rust yet.

### Next

**M1 — the first vertical slice:** scaffold `main.rs`/`app.rs`, the token
setup screen writing to the keyring, build+verify an octocrab client, a repo
picker to choose the watch list, and a single list of recent runs across it with
status chips and a header count — polled on an interval that pauses when hidden,
with errors that name their fix. Each octocrab and oo7 call verified against
docs.rs as it's written (CLAUDE.md rules 1 and 2), not from memory.

The remaining milestones (M2–M7) are in CLAUDE.md's Scope section.
