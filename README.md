<h1 align="center">Pitwall</h1>

<p align="center">
  A native GNOME app to monitor your GitHub Actions runs from the desktop.
</p>

<p align="center">
  <img src="https://img.shields.io/badge/Rust-2024-CE422B?logo=rust&logoColor=white" alt="Rust 2024">
  <img src="https://img.shields.io/badge/GTK4-libadwaita-4A90D9?logo=gnome&logoColor=white" alt="GTK4 + libadwaita">
  <img src="https://img.shields.io/badge/platform-Linux%20%2F%20GNOME-303030" alt="Platform: Linux / GNOME">
  <img src="https://img.shields.io/badge/license-GPL--3.0--or--later-blue" alt="License: GPL-3.0-or-later">
  <img src="https://img.shields.io/badge/status-planning-yellow" alt="Status: planning">
</p>

---

Pitwall watches the GitHub Actions runs of a hand-picked set of your repos and
keeps you on top of them from a window that looks and behaves like a first-party
GNOME application — a live list of runs with colour-coded status, drill-down into
jobs, steps and logs, one-tap **re-run** / **cancel**, and — the point of it — a
native desktop **notification the moment a run fails** while you're doing
something else.

The name is the motorsport pit wall: where the team sits watching live timing as
the race runs and calls strategy. It's the sibling of
[**Dockyard**](https://github.com/SoftARV/Dockyard) and shares its stack:
[relm4](https://relm4.org/) (the Elm architecture, in Rust),
[GTK 4](https://www.gtk.org/) + [libadwaita](https://gnome.pages.gitlab.gnome.org/libadwaita/),
with [octocrab](https://docs.rs/octocrab/) for the GitHub API and
[oo7](https://docs.rs/oo7/) to keep your token in the GNOME keyring.

> **Status: planning / early scaffold.** The design is settled (see below) and
> the repo is scaffolded; the app isn't built yet. Milestones are in `CLAUDE.md`.

## Planned features (v1)

- **Run list** — recent workflow runs across your watched repos, newest first,
  each with a colour-coded status chip (success / failure / running / cancelled /
  queued), the repo, branch, event and time; a live count in the header.
- **Failure notifications** — a native GNOME desktop notification when a watched
  run fails, clickable straight to that run. Configurable (failures / all / off).
- **Run detail** — jobs and their steps with per-step status and duration, plus
  the run's branch, commit, actor and elapsed time.
- **Logs** — a completed job's log in a fixed-dark terminal panel. (GitHub's API
  has no live log stream, so running jobs offer "Open in browser" instead.)
- **Actions** — re-run, re-run failed jobs, and cancel, right from the app.
- **Native and adaptive** — libadwaita throughout, so light/dark, the system
  accent, and adaptive layout come for free.
- **Light on the rate limit** — a conditional-request poll that pauses entirely
  while the window is hidden, so a backgrounded Pitwall costs almost nothing
  against your 5000-requests/hour budget.
- **Your token stays secret** — stored in the GNOME keyring via the Secret
  Service, never in a config file.

## Requirements

- **Rust** ≥ 1.93 (edition 2024)
- **GTK** ≥ 4.20 and **libadwaita** ≥ 1.8
- A running **keyring** provider (gnome-keyring is default on GNOME)
- A GitHub **fine-grained token** (`Actions: read`; add write for re-run/cancel)

On Arch / CachyOS:

```bash
sudo pacman -S --needed base-devel pkgconf rust gtk4 libadwaita librsvg
```

## Build and run

```bash
cargo run                       # dev build, launches the window
RUST_LOG=pitwall=debug cargo run
cargo build --release           # optimised binary at target/release/pitwall
```

## How it works

Pitwall is Redux with a compiler: a single model, one reducer, and a view
derived from state — relm4's take on the Elm architecture. All GitHub I/O runs
off the GTK main thread through relm4 commands, and octocrab's types are mapped
to our own at the boundary so the UI never touches them.

- **[ARCHITECTURE.md](ARCHITECTURE.md)** — the explanation: the pieces, why
  they're shaped that way, and how it differs from its sibling Dockyard.
- **[CLAUDE.md](CLAUDE.md)** — the rulebook: the pinned stack and the hard rules.

## License

Pitwall is free software, licensed under the **GNU General Public License v3.0
or later** (GPL-3.0-or-later). See [COPYING](COPYING) for the full text, or
<https://www.gnu.org/licenses/gpl-3.0.html>.

Copyright © 2026 Miguel Rincon.
