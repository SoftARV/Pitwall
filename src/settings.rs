// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Persistent global preferences.
//!
//! A small INI file in the XDG config dir (`~/.config/pitwall/settings.ini`),
//! read through `glib::KeyFile`. Deliberately *not* GSettings — that needs a
//! compiled schema installed before the app will even start, breaking
//! `cargo run` — and, per CLAUDE.md rule 2, deliberately *not* where the GitHub
//! token goes: the token is a secret and lives in the keyring (`secret.rs`),
//! never here.
//!
//! Holds the theme and the watched-repo list; later milestones add the poll
//! interval and notification preferences.

use std::path::PathBuf;

use relm4::adw;
use relm4::gtk::glib;

/// The window's colour scheme: follow the desktop, or force light/dark.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Theme {
    /// Follow the system light/dark preference.
    #[default]
    System,
    Light,
    Dark,
}

impl Theme {
    /// The libadwaita colour scheme this maps to. `Force*` overrides the system;
    /// `Default` follows it.
    fn color_scheme(self) -> adw::ColorScheme {
        match self {
            Theme::System => adw::ColorScheme::Default,
            Theme::Light => adw::ColorScheme::ForceLight,
            Theme::Dark => adw::ColorScheme::ForceDark,
        }
    }

    /// The stable string written to the config file.
    fn as_key(self) -> &'static str {
        match self {
            Theme::System => "system",
            Theme::Light => "light",
            Theme::Dark => "dark",
        }
    }

    fn from_key(key: &str) -> Self {
        match key {
            "light" => Theme::Light,
            "dark" => Theme::Dark,
            _ => Theme::System,
        }
    }
}

/// The app's global settings. Loaded once at startup, saved on every change.
#[derive(Debug, Clone, Default)]
pub struct Settings {
    pub theme: Theme,
    /// The hand-picked repos to monitor, as `"owner/name"`. Empty until the user
    /// adds some via the repo picker.
    pub watched: Vec<String>,
}

impl Settings {
    /// Load from disk, falling back to the defaults for a missing file or any
    /// missing/malformed key — a broken config should never stop the app.
    pub fn load() -> Self {
        let mut settings = Self::default();

        let keyfile = glib::KeyFile::new();
        if keyfile
            .load_from_file(config_path(), glib::KeyFileFlags::NONE)
            .is_err()
        {
            // No file yet (first run), or unreadable — defaults it is.
            return settings;
        }

        if let Ok(theme) = keyfile.string("appearance", "theme") {
            settings.theme = Theme::from_key(&theme);
        }
        // Stored comma-joined. A repo full name is `owner/name` and can't contain
        // a comma, so a plain split round-trips exactly; the empty-string filter
        // turns "" (no repos) back into an empty list rather than `[""]`.
        if let Ok(repos) = keyfile.string("watchlist", "repos") {
            settings.watched = repos
                .split(',')
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
                .collect();
        }

        settings
    }

    /// Write to disk, creating the config directory if needed. Failures are
    /// logged, not fatal — losing a preference isn't worth crashing over.
    pub fn save(&self) {
        let keyfile = glib::KeyFile::new();
        keyfile.set_string("appearance", "theme", self.theme.as_key());
        keyfile.set_string("watchlist", "repos", &self.watched.join(","));

        let path = config_path();
        if let Some(dir) = path.parent()
            && let Err(err) = std::fs::create_dir_all(dir)
        {
            tracing::warn!(%err, "couldn't create the config directory");
            return;
        }
        if let Err(err) = keyfile.save_to_file(&path) {
            tracing::warn!(%err, "couldn't save settings");
        }
    }

    /// Apply the theme to the whole app, globally via `adw::StyleManager`. Call
    /// at startup (before the window shows, so there's no flash of the wrong
    /// scheme) and whenever the theme changes.
    pub fn apply_theme(&self) {
        adw::StyleManager::default().set_color_scheme(self.theme.color_scheme());
    }
}

fn config_path() -> PathBuf {
    glib::user_config_dir().join("pitwall").join("settings.ini")
}
