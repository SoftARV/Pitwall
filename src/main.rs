// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

mod app;
mod components;
mod github;
mod secret;
mod settings;

use relm4::RelmApp;
use relm4::gtk;
use relm4::gtk::gdk;
use tracing_subscriber::EnvFilter;

pub(crate) const APP_ID: &str = "dev.miguelrincon.Pitwall";

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("pitwall=debug")),
        )
        .init();

    // `RelmApp::new` calls `gtk::init()` and — because we enable relm4's
    // `libadwaita` feature — `adw::init()` too, and builds an `adw::Application`.
    // So there's deliberately no adw init here.
    let app = RelmApp::new(APP_ID);
    setup_icon();
    // Install CSS once GTK is up: the GitHub-brand sign-in button, and the
    // shared status chip (each owns its own stylesheet).
    app::install_css();
    components::status_chip::install_css();
    components::log_view::install_css();

    // Load persisted settings and apply the theme before the window is shown, so
    // there's no flash of the wrong colour scheme. The model owns them from here.
    let settings = settings::Settings::load();
    settings.apply_theme();
    app.run::<app::AppModel>(settings);
}

/// Point GTK at our icon and name it as the default.
///
/// On Wayland this does **not** put an icon on the window — a client can't set
/// its own toplevel icon there; GNOME Shell takes it from the installed
/// `.desktop`, so only the installed app shows one. It's kept because it's the
/// standard idiom and does work on X11 and some compositors, and the search
/// path lets a dev build resolve the icon pre-install. Harmless no-ops on
/// Wayland. Must run after `RelmApp::new`, which initialised GTK.
fn setup_icon() {
    if let Some(display) = gdk::Display::default() {
        let theme = gtk::IconTheme::for_display(&display);
        theme.add_search_path(concat!(env!("CARGO_MANIFEST_DIR"), "/data/icons"));
    }
    gtk::Window::set_default_icon_name(APP_ID);
}
