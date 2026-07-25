<h1 align="center">Pitwall</h1>

<p align="center">
  A native GNOME app to monitor your GitHub Actions runs from the desktop.
</p>

<p align="center">
  <img src="https://img.shields.io/badge/Rust-2024-CE422B?logo=rust&logoColor=white" alt="Rust 2024">
  <img src="https://img.shields.io/badge/GTK4-libadwaita-4A90D9?logo=gnome&logoColor=white" alt="GTK4 + libadwaita">
  <img src="https://img.shields.io/badge/platform-Linux%20%2F%20GNOME-303030" alt="Platform: Linux / GNOME">
  <img src="https://img.shields.io/badge/license-GPL--3.0--or--later-blue" alt="License: GPL-3.0-or-later">
</p>

---

Pitwall watches the GitHub Actions runs of a hand-picked set of your repos and
keeps you on top of them from a window that looks and behaves like a first-party
GNOME application — a live list of runs with colour-coded status, drill-down into
jobs, steps, logs and annotations, one-click **re-run** / **cancel**, and — the
point of it — a native desktop **notification the moment a run fails** while
you're doing something else.

The name is the motorsport pit wall: where the team sits watching live timing as
the race runs and calls strategy. It's the sibling of
[**Dockyard**](https://github.com/SoftARV/Dockyard) and shares its stack:
[relm4](https://relm4.org/) (the Elm architecture, in Rust),
[GTK 4](https://www.gtk.org/) + [libadwaita](https://gnome.pages.gitlab.gnome.org/libadwaita/),
with [octocrab](https://docs.rs/octocrab/) for the GitHub API and
[oo7](https://docs.rs/oo7/) to keep your token in the GNOME keyring.

> Pitwall is a personal, single-machine **monitor** — not a full GitHub client,
> not multi-user, not cross-platform. One user, one Linux laptop.

## Features

- **Recent & Repositories views** — a **Recent** tab with the newest runs across
  your watched repos (newest first, colour-coded status chip, repo · branch ·
  event · time, and a failing-count badge), and a **Repositories** tab where each
  watched repo shows its latest run and expands to that repo's history.
- **Failure notifications** — a native GNOME desktop notification when a watched
  run finishes, clickable straight to that run. Configurable: failures only, every
  run, or off.
- **Run detail** — jobs and their steps with per-step status and duration, the
  run's branch, commit, event and elapsed time, and an **Annotations** section
  surfacing the warnings / errors / notices GitHub attaches to the run. Stays live
  while it's open.
- **Logs** — a completed job's full log in a fixed-dark terminal panel, with
  collapsible `##[group]` sections and the tools' ANSI colour preserved. (GitHub's
  API has no live log stream, so a running job offers "Open in browser" instead.)
- **Actions** — re-run, re-run failed jobs, and cancel (with a confirmation),
  right from the list or the detail page, with optimistic feedback so the status
  updates immediately rather than waiting on GitHub's API.
- **Search** — filter the runs by repo, workflow or branch (Ctrl+F, or just start
  typing).
- **Preferences** — theme (follow system / light / dark), background poll
  interval, and notification level.
- **Native and adaptive** — libadwaita throughout, so light/dark, the system
  accent, adaptive layout and keyboard shortcuts (Ctrl+?) come for free.
- **Light on the rate limit** — a conditional-request (ETag) poll that pauses
  entirely while the window is hidden, so a backgrounded Pitwall costs almost
  nothing against your 5000-requests/hour budget.
- **Your token stays secret** — obtained via GitHub's OAuth device flow and stored
  in the GNOME keyring via the Secret Service, never in a config file or a log.

## Requirements

- **Rust** ≥ 1.93 (edition 2024)
- **GTK** ≥ 4.20 and **libadwaita** ≥ 1.8
- A running **keyring** provider (gnome-keyring is the default on GNOME)
- A **GitHub account** — no token to create or paste; you sign in from the app

On Arch / CachyOS:

```bash
sudo pacman -S --needed base-devel pkgconf rust gtk4 libadwaita librsvg
```

## Install

Installs the binary, the `.desktop` entry and the icons under `~/.local` (no
sudo needed — it's a per-user, one-machine app):

```bash
make install     # → launch "Pitwall" from the app grid, or run `pitwall`
make uninstall
```

For a system-wide install: `sudo make PREFIX=/usr/local install`.

> **Notifications need the installed app.** GNOME's notification backend only
> shows notifications for an app it can resolve to an installed `.desktop` entry,
> so run `make install` (and, the first time, log out/in so GNOME Shell picks up
> the new icon) for failure alerts to appear.

## Build and run (development)

```bash
cargo run                       # dev build, launches the window
RUST_LOG=pitwall=debug cargo run
cargo build --release           # optimised binary at target/release/pitwall

make check                      # the bar: fmt --check, clippy -D warnings, test
```

## First run

On first launch a blocking onboarding dialog introduces the app and offers
**Sign in with GitHub**. Pitwall uses GitHub's OAuth **device flow**: it shows an
8-character code, opens `github.com/login/device` in your browser (copying the
code for you), and connects once you authorise. The `repo` scope is requested —
the narrowest that reads Actions on your private and public repos and later
re-runs / cancels them. Then pick the repositories to watch, and you're set.

## Configuration

- **Preferences** (Ctrl+, or the primary menu): theme, poll interval, and when to
  notify. Persisted to `~/.config/pitwall/settings.ini` (also holds your watched
  repo list and theme — **never** the token).
- **Token**: the GitHub OAuth token lives in the GNOME keyring, keyed by the app
  id. Sign out from the menu to clear it.

## Keyboard shortcuts

| Shortcut        | Action              |
| --------------- | ------------------- |
| `Ctrl`+`R` / `F5` | Refresh           |
| `Ctrl`+`F`      | Search              |
| `Ctrl`+`,`      | Preferences         |
| `Ctrl`+`?`      | Keyboard shortcuts  |
| `Ctrl`+`Q`      | Quit                |

## How it works

Pitwall is Redux with a compiler: a single model, one reducer, and a view derived
from state — relm4's take on the Elm architecture. All GitHub I/O runs off the
GTK main thread through relm4 commands, and octocrab's types are mapped to our own
at the boundary so the UI never touches them.

- **[ARCHITECTURE.md](ARCHITECTURE.md)** — the explanation: the pieces, why
  they're shaped that way, and how it differs from its sibling Dockyard.
- **[CLAUDE.md](CLAUDE.md)** — the rulebook: the pinned stack and the hard rules.

## License

Pitwall is free software, licensed under the **GNU General Public License v3.0
or later** (GPL-3.0-or-later). See [COPYING](COPYING) for the full text, or
<https://www.gnu.org/licenses/gpl-3.0.html>.

Copyright © 2026 Miguel Rincon.
