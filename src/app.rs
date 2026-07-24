// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Root component.
//!
//! Redux with a compiler: `AppMsg` are the actions, `update` is the sole
//! reducer, and the view is derived from `AppModel`. Nothing here does I/O
//! inline — every keyring and GitHub call is dispatched as a relm4 `Command` so
//! the GTK main thread never blocks (CLAUDE.md rules 4 and 5).
//!
//! This milestone (M1, PR 1) is the auth slice. Sign-in is the OAuth **device
//! flow**, presented in a **blocking onboarding modal** so the app can't be used
//! until you're connected: a first-run intro → "Sign in with GitHub" → a user
//! code + browser authorisation → the modal closes onto the app. A saved token
//! in the keyring reconnects silently at startup, skipping the modal entirely.

use futures_util::FutureExt;
use relm4::actions::{AccelsPlus, RelmAction, RelmActionGroup};
use relm4::adw::prelude::*;
use relm4::factory::FactoryVecDeque;
use relm4::gtk::{gio, glib};
use relm4::{
    Component, ComponentController, ComponentParts, ComponentSender, Controller, RelmWidgetExt,
    adw, gtk,
};

use crate::components::repo_picker::{RepoPicker, RepoPickerInit, RepoPickerOutput};
use crate::components::run_row::{RunRow, RunRowInput, RunRowOutput};
use crate::github::client::{self, ConnectError, Connection};
use crate::github::types::{Conclusion, WorkflowRun};
use crate::secret;
use crate::settings::Settings;

// The primary menu's action group. GTK menu items invoke `GAction`s by name;
// this defines the "win" group and its stateless actions (fully qualified, e.g.
// `win.about`). The group is registered on the window in `init`, where each
// action bridges to an `AppMsg` (except Quit, which acts directly).
relm4::new_action_group!(AppMenuActionGroup, "win");
relm4::new_stateless_action!(RefreshAction, AppMenuActionGroup, "refresh");
relm4::new_stateless_action!(EditWatchlistAction, AppMenuActionGroup, "edit-watchlist");
relm4::new_stateless_action!(SignOutAction, AppMenuActionGroup, "sign-out");
relm4::new_stateless_action!(AboutAction, AppMenuActionGroup, "about");
relm4::new_stateless_action!(QuitAction, AppMenuActionGroup, "quit");

/// Poll interval for the run list. 45s is CLAUDE.md's default; the timer is
/// removed entirely while the window is hidden (rule 3), so a backgrounded
/// Pitwall costs nothing against the rate budget.
const POLL_INTERVAL_SECS: u32 = 45;

// The one bit of custom CSS: a black "Sign in with GitHub" button, so it's
// instantly recognisable and stands out on the modal (the earlier grey blended
// in). Deliberately theme-*in*dependent — black with white text in both light and
// dark mode — so it's a fixed colour, not an Adwaita named one. The hover/active
// shades lighten for press feedback. Installed once from `main` (CLAUDE.md: CSS
// lives with the code that uses it).
const CSS: &str = "
.github-button { background-color: #000000; color: #ffffff; }
.github-button:hover { background-color: #2c2c2c; }
.github-button:active { background-color: #444444; }
";

/// Install the app's CSS. Called from `main` once GTK is up.
pub fn install_css() {
    relm4::set_global_css(CSS);
}

/// The screen the main window shows (behind the onboarding modal, when that's up).
#[derive(Debug)]
pub enum ViewState {
    /// Verifying a saved token at startup.
    Loading,
    /// Not signed in — the onboarding modal is presented over a neutral backdrop.
    SignedOut,
    /// Connected and verified. The repo picker / run list land here next.
    Ready,
    /// Couldn't connect for a non-auth reason (offline, 403, …). Carries the
    /// message, which names the fix.
    Disconnected(String),
}

/// Handles to the onboarding modal, driven imperatively. The modal isn't part of
/// the window's widget tree — it's *presented* over it — so `#[watch]` can't
/// reach it; we hold the pieces we need to update (the stack page and the code)
/// and mutate them from the reducer, the same imperative-handle escape hatch
/// Dockyard uses for `nav` / `toast_overlay`. Built fresh each time we need to
/// sign in, dropped on success.
struct Onboarding {
    dialog: adw::Dialog,
    stack: gtk::Stack,
    code_label: gtk::Label,
    toast_overlay: adw::ToastOverlay,
}

pub struct AppModel {
    connection: Option<Connection>,
    state: ViewState,
    /// The current device-flow (user_code, verification_uri), for the copy/open
    /// buttons. `Some` only while a sign-in is in flight.
    device: Option<(String, String)>,
    /// Bumped on every sign-in and every cancel, and stamped onto each device
    /// command. A result whose generation no longer matches is from a flow the
    /// user has since cancelled or superseded, so it's ignored — this is what
    /// keeps an orphaned poll from knocking us out of a good state.
    auth_gen: u64,
    toast_overlay: adw::ToastOverlay,
    /// The onboarding modal, while it's shown.
    onboarding: Option<Onboarding>,
    /// The repo picker dialog, while it's open.
    repo_picker: Option<Controller<RepoPicker>>,
    /// The watched repos as `owner/name`, mirrored from settings for the view.
    watched: Vec<String>,
    /// The visible run rows, reconciled in place from `all_runs` (rule 7).
    runs: FactoryVecDeque<RunRow>,
    /// The full run set from the last poll — what the header count and the
    /// reconcile read.
    all_runs: Vec<WorkflowRun>,
    /// Whether a poll has landed since connecting: tells "still loading" apart
    /// from "genuinely no runs".
    runs_loaded: bool,
    /// A refresh the user asked for (menu / Ctrl+R): shows the header spinner.
    /// The background poll is silent.
    refreshing: bool,
    /// The poll timer, while running. Removed outright when the window is hidden.
    poll: Option<glib::SourceId>,
    /// The "Sign Out", "Edit Watchlist", and "Refresh" menu actions — all
    /// `hidden-when="action-disabled"`, so they show (and act) only when signed
    /// in; disabling *hides* them rather than greying them.
    signout_action: gio::SimpleAction,
    editwatch_action: gio::SimpleAction,
    refresh_action: gio::SimpleAction,
    settings: Settings,
}

#[derive(Debug, Clone)]
pub enum AppMsg {
    /// "Sign in with GitHub" (from the onboarding modal) — start the device flow.
    SignIn,
    /// Re-open github.com/login/device in the browser.
    OpenVerification,
    /// Copy the current user code to the clipboard.
    CopyCode,
    /// Abandon the in-progress sign-in and return to the intro.
    CancelAuth,
    /// "Sign Out" — forget the token and return to onboarding.
    SignOut,
    /// Open the repo picker (menu item or empty-state button).
    EditWatchlist,
    /// The picker saved a new watch set.
    WatchlistSaved(Vec<String>),
    /// "Try Again" on the disconnected page — re-run the startup connect.
    Retry,
    /// The background poll tick (silent) — reload the runs.
    Refresh,
    /// A user-triggered refresh (menu / Ctrl+R): reload, and spin the header.
    ManualRefresh,
    /// Open a run on github.com (a row was activated).
    OpenInBrowser(String),
    /// The window became visible / hidden — gates the poll (rule 3).
    SuspendedChanged(bool),
    /// Open the About dialog (primary menu).
    ShowAbout,
}

/// Results coming back from commands — off-thread work landing on the main
/// thread. Separate from `AppMsg` because relm4 gives commands their own channel
/// (this is the `CommandOutput` associated type).
#[derive(Debug)]
pub enum CommandMsg {
    /// The startup connect finished (saved token from the keyring).
    Connected(Box<Result<Connection, ConnectError>>),
    /// Startup found no saved token.
    NoToken,
    /// The token was cleared from the keyring.
    TokenCleared,
    /// Device flow step 1: GitHub returned a user code to show.
    DeviceCode {
        generation: u64,
        user_code: String,
        verification_uri: String,
    },
    /// Device flow finished: authorised (and verified) or failed.
    DeviceResult {
        generation: u64,
        result: Box<Result<Connection, ConnectError>>,
    },
    /// A poll returned the runs across the watched repos.
    RunsLoaded(Vec<WorkflowRun>),
    /// A poll failed for every watched repo (offline / bad token).
    RunsFailed(String),
}

impl AppModel {
    /// Which main-window `Stack` page to show for the current state.
    fn state_page(&self) -> &'static str {
        match self.state {
            ViewState::Loading => "loading",
            ViewState::SignedOut => "welcome",
            ViewState::Ready => "app",
            ViewState::Disconnected(_) => "disconnected",
        }
    }

    fn toast(&self, message: &str) {
        self.toast_overlay.add_toast(adw::Toast::new(message));
    }

    /// The header subtitle: a live run summary once runs have loaded, else who
    /// we're signed in as. Empty when disconnected, so the subtitle line hides.
    fn header_subtitle(&self) -> String {
        if matches!(self.state, ViewState::Ready) && self.runs_loaded && !self.all_runs.is_empty() {
            let total = self.all_runs.len();
            let failing = self
                .all_runs
                .iter()
                .filter(|run| run.conclusion.is_some_and(Conclusion::is_failure))
                .count();
            let running = self
                .all_runs
                .iter()
                .filter(|run| run.status.is_active())
                .count();
            let mut parts = vec![format!("{total} run{}", if total == 1 { "" } else { "s" })];
            if failing > 0 {
                parts.push(format!("{failing} failing"));
            }
            if running > 0 {
                parts.push(format!("{running} running"));
            }
            parts.join(" · ")
        } else if let Some(connection) = &self.connection {
            format!("Signed in as {}", connection.login)
        } else {
            String::new()
        }
    }

    /// Enable (or disable) the signed-in-only menu actions together. Disabled
    /// means hidden, via each item's `hidden-when="action-disabled"`.
    fn set_signed_in(&self, signed_in: bool) {
        self.signout_action.set_enabled(signed_in);
        self.editwatch_action.set_enabled(signed_in);
        self.refresh_action.set_enabled(signed_in);
    }

    /// Which `app` sub-page to show: no repos, first-load spinner, no runs, or
    /// the list.
    fn app_page(&self) -> &'static str {
        if self.watched.is_empty() {
            "no-repos"
        } else if !self.runs_loaded {
            "loading"
        } else if self.all_runs.is_empty() {
            "no-runs"
        } else {
            "runs"
        }
    }

    /// Start the poll, unless it's already running or there's nothing to poll.
    /// Idempotent, so a `SuspendedChanged(false)` arriving while it already runs
    /// doesn't stack a second timer.
    fn start_poll(&mut self, sender: &ComponentSender<Self>) {
        if self.poll.is_some() || self.connection.is_none() || self.watched.is_empty() {
            return;
        }
        let input = sender.input_sender().clone();
        self.poll = Some(glib::timeout_add_seconds_local(
            POLL_INTERVAL_SECS,
            move || {
                input.send(AppMsg::Refresh).ok();
                glib::ControlFlow::Continue
            },
        ));
    }

    /// Remove the poll timer entirely, so it stops waking the CPU (and network).
    /// `take()` matters: removing a `SourceId` twice is a glib programmer error.
    fn stop_poll(&mut self) {
        if let Some(source) = self.poll.take() {
            source.remove();
        }
    }

    /// Fire off a run-list fetch across the watched repos, off-thread.
    fn dispatch_refresh(&self, sender: &ComponentSender<Self>) {
        let Some(connection) = &self.connection else {
            return;
        };
        if self.watched.is_empty() {
            return;
        }
        // `Octocrab` is Arc-backed, so this clone is a pointer bump — and the
        // command needs `'static + Send` data it owns, not a borrow of `self`.
        let octocrab = connection.octocrab.clone();
        let repos = self.watched.clone();
        sender.oneshot_command(async move {
            match client::list_runs(&octocrab, &repos).await {
                Ok(runs) => CommandMsg::RunsLoaded(runs),
                Err(reason) => CommandMsg::RunsFailed(reason),
            }
        });
    }

    /// Reconcile the visible rows from `all_runs`: update in place while the id
    /// set is unchanged, rebuild only when membership changes (rule 7). The clone
    /// avoids a simultaneous borrow of `self.all_runs` and `self.runs`.
    fn apply_runs(&mut self) {
        let target = self.all_runs.clone();
        let unchanged = self.runs.len() == target.len()
            && self
                .runs
                .iter()
                .zip(&target)
                .all(|(row, run)| row.id() == run.id);

        if unchanged {
            for (index, run) in target.into_iter().enumerate() {
                self.runs.send(index, RunRowInput::Update(run));
            }
        } else {
            let mut guard = self.runs.guard();
            guard.clear();
            for run in target {
                guard.push_back(run);
            }
        }
    }

    /// The disconnected page's description — the reason we couldn't connect.
    fn disconnected_reason(&self) -> String {
        match &self.state {
            ViewState::Disconnected(reason) => reason.clone(),
            _ => String::new(),
        }
    }

    /// Open a URL in the user's browser. `gtk::UriLauncher` is the modern,
    /// non-deprecated way (the old `gtk::show_uri` is deprecated and would fail
    /// `clippy -D warnings`). The callback is required but there's nothing to do
    /// with the result.
    fn open_uri(uri: &str, root: &adw::ApplicationWindow) {
        gtk::UriLauncher::new(uri).launch(Some(root), gio::Cancellable::NONE, |_result| {});
    }

    /// Build the onboarding modal: an `adw::Dialog` (non-closable, so it blocks
    /// the app until sign-in) wrapping a two-page stack — the intro and the
    /// waiting-for-authorisation page. Buttons post `AppMsg`s so the flow still
    /// runs through the one reducer.
    fn build_onboarding(sender: &ComponentSender<Self>) -> Onboarding {
        // A click-to-message helper, so each button is one line.
        let bridge = |msg: AppMsg| {
            let input = sender.input_sender().clone();
            move |_: &gtk::Button| {
                input.send(msg.clone()).ok();
            }
        };

        // --- Intro page ---
        let intro = gtk::Box::new(gtk::Orientation::Vertical, 12);
        intro.set_valign(gtk::Align::Center);
        intro.set_halign(gtk::Align::Center);
        intro.set_margin_all(36);

        let icon = gtk::Image::from_icon_name(crate::APP_ID);
        icon.set_pixel_size(96);
        intro.append(&icon);

        let title = gtk::Label::new(Some("Welcome to Pitwall"));
        title.add_css_class("title-1");
        intro.append(&title);

        let desc = gtk::Label::new(Some(
            "Keep an eye on your GitHub Actions. Pitwall watches your repositories’ \
             workflow runs and lets you know the moment one fails — a native GNOME \
             monitor for your CI.",
        ));
        desc.set_wrap(true);
        desc.set_justify(gtk::Justification::Center);
        desc.set_max_width_chars(38);
        desc.add_css_class("dim-label");
        intro.append(&desc);

        let signin = gtk::Button::with_label("Sign in with GitHub");
        signin.set_halign(gtk::Align::Center);
        signin.set_margin_top(12);
        signin.add_css_class("github-button");
        signin.add_css_class("pill");
        signin.connect_clicked(bridge(AppMsg::SignIn));
        intro.append(&signin);

        // --- Awaiting-authorisation page ---
        let awaiting = gtk::Box::new(gtk::Orientation::Vertical, 18);
        awaiting.set_valign(gtk::Align::Center);
        awaiting.set_halign(gtk::Align::Center);
        awaiting.set_margin_all(36);

        let a_title = gtk::Label::new(Some("Authorize Pitwall"));
        a_title.add_css_class("title-2");
        awaiting.append(&a_title);

        let a_desc = gtk::Label::new(Some(
            "Enter this code at github.com/login/device — we opened it in your browser.",
        ));
        a_desc.set_wrap(true);
        a_desc.set_justify(gtk::Justification::Center);
        a_desc.set_max_width_chars(38);
        a_desc.add_css_class("dim-label");
        awaiting.append(&a_desc);

        let code_label = gtk::Label::new(None);
        code_label.add_css_class("title-1");
        code_label.add_css_class("numeric");
        code_label.set_selectable(true);
        awaiting.append(&code_label);

        let code_buttons = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        code_buttons.set_halign(gtk::Align::Center);
        let open = gtk::Button::with_label("Open GitHub");
        open.add_css_class("pill");
        open.connect_clicked(bridge(AppMsg::OpenVerification));
        let copy = gtk::Button::with_label("Copy Code");
        copy.add_css_class("pill");
        copy.connect_clicked(bridge(AppMsg::CopyCode));
        code_buttons.append(&open);
        code_buttons.append(&copy);
        awaiting.append(&code_buttons);

        let waiting = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        waiting.set_halign(gtk::Align::Center);
        let spinner = gtk::Spinner::new();
        spinner.set_spinning(true);
        waiting.append(&spinner);
        let waiting_label = gtk::Label::new(Some("Waiting for authorization…"));
        waiting_label.add_css_class("dim-label");
        waiting.append(&waiting_label);
        awaiting.append(&waiting);

        let cancel = gtk::Button::with_label("Cancel");
        cancel.set_halign(gtk::Align::Center);
        cancel.add_css_class("flat");
        cancel.connect_clicked(bridge(AppMsg::CancelAuth));
        awaiting.append(&cancel);

        // --- Stack + dialog ---
        let stack = gtk::Stack::new();
        stack.add_named(&intro, Some("intro"));
        stack.add_named(&awaiting, Some("awaiting"));
        stack.set_visible_child_name("intro");

        let toast_overlay = adw::ToastOverlay::new();
        toast_overlay.set_child(Some(&stack));

        let dialog = adw::Dialog::new();
        // Can't be dismissed: no close button, Escape does nothing — you sign in
        // or you quit the app. `force_close` (on success) overrides this.
        dialog.set_can_close(false);
        dialog.set_content_width(400);
        dialog.set_child(Some(&toast_overlay));

        Onboarding {
            dialog,
            stack,
            code_label,
            toast_overlay,
        }
    }

    /// Present the onboarding modal (building it if needed) and mark us signed
    /// out. Idempotent — a second call just resets it to the intro.
    fn show_onboarding(&mut self, root: &adw::ApplicationWindow, sender: &ComponentSender<Self>) {
        match &self.onboarding {
            None => {
                let onboarding = Self::build_onboarding(sender);
                onboarding.dialog.present(Some(root));
                self.onboarding = Some(onboarding);
            }
            Some(onboarding) => {
                onboarding.stack.set_visible_child_name("intro");
            }
        }
        self.state = ViewState::SignedOut;
        self.device = None;
        self.set_signed_in(false);
    }

    /// Close the modal (if any), move to the connected app, and start polling.
    fn go_ready(&mut self, connection: Connection, sender: &ComponentSender<Self>) {
        if let Some(onboarding) = self.onboarding.take() {
            onboarding.dialog.force_close();
        }
        self.connection = Some(connection);
        self.state = ViewState::Ready;
        self.device = None;
        self.runs_loaded = false;
        self.set_signed_in(true);
        // A fresh connection: poll now and on the interval (both no-op if the
        // watch list is empty).
        self.start_poll(sender);
        self.dispatch_refresh(sender);
    }

    /// Load a saved token off-thread and, if there is one, build and verify a
    /// client from it. Associated rather than a method because `init` and
    /// `Retry` both need it, and `init` has no model to call it on yet.
    fn dispatch_boot(sender: &ComponentSender<Self>) {
        sender.oneshot_command(async move {
            match secret::load().await {
                Ok(Some(token)) => CommandMsg::Connected(Box::new(client::connect(token).await)),
                Ok(None) => CommandMsg::NoToken,
                Err(err) => CommandMsg::Connected(Box::new(Err(ConnectError::Other(format!(
                    "Couldn't read the keyring: {err:#}"
                ))))),
            }
        });
    }
}

#[relm4::component(pub)]
impl Component for AppModel {
    type Init = Settings;
    type Input = AppMsg;
    type Output = ();
    type CommandOutput = CommandMsg;

    view! {
        adw::ApplicationWindow {
            set_title: Some("Pitwall"),
            set_default_size: (900, 720),

            #[local_ref]
            toast_overlay -> adw::ToastOverlay {
                adw::ToolbarView {
                    add_top_bar = &adw::HeaderBar {
                        #[wrap(Some)]
                        set_title_widget = &adw::WindowTitle {
                            set_title: "Pitwall",
                            #[watch]
                            set_subtitle: &model.header_subtitle(),
                        },

                        // The menu model is built imperatively in `init` (so the
                        // Sign Out item can carry `hidden-when="action-disabled"`)
                        // and set on this button there.
                        #[name = "menu_button"]
                        pack_end = &gtk::MenuButton {
                            set_icon_name: "open-menu-symbolic",
                            set_tooltip_text: Some("Main Menu"),
                        },

                        // Spins only for a refresh the user asked for; the
                        // background poll is silent.
                        pack_end = &gtk::Spinner {
                            #[watch]
                            set_visible: model.refreshing,
                            #[watch]
                            set_spinning: model.refreshing,
                            set_valign: gtk::Align::Center,
                        },
                    },

                    // One page per state. Onboarding (sign-in) is a modal over
                    // this, not a page here — so this stack is just loading /
                    // signed-out backdrop / the app / disconnected.
                    #[wrap(Some)]
                    set_content = &gtk::Stack {
                        add_named[Some("loading")] = &gtk::Box {
                            set_orientation: gtk::Orientation::Vertical,
                            set_valign: gtk::Align::Center,
                            set_halign: gtk::Align::Center,
                            gtk::Spinner {
                                set_spinning: true,
                                set_size_request: (32, 32),
                            },
                        },

                        // The neutral backdrop behind the onboarding modal.
                        add_named[Some("welcome")] = &adw::StatusPage {
                            set_icon_name: Some(crate::APP_ID),
                            set_title: "Pitwall",
                            set_description: Some("Sign in to start monitoring your workflow runs."),
                        },

                        // The connected app: a nested stack over the run list.
                        add_named[Some("app")] = &gtk::Stack {
                            add_named[Some("no-repos")] = &adw::StatusPage {
                                set_icon_name: Some("view-list-symbolic"),
                                set_title: "No repositories watched",
                                set_description: Some(
                                    "Add the repositories whose Actions you want to keep an eye on.",
                                ),

                                #[wrap(Some)]
                                set_child = &gtk::Box {
                                    set_halign: gtk::Align::Center,

                                    #[name = "watchlist_button"]
                                    gtk::Button {
                                        set_label: "Add Repositories",
                                        add_css_class: "pill",
                                        add_css_class: "suggested-action",
                                    },
                                },
                            },

                            add_named[Some("loading")] = &gtk::Box {
                                set_orientation: gtk::Orientation::Vertical,
                                set_valign: gtk::Align::Center,
                                set_halign: gtk::Align::Center,
                                gtk::Spinner {
                                    set_spinning: true,
                                    set_size_request: (32, 32),
                                },
                            },

                            add_named[Some("no-runs")] = &adw::StatusPage {
                                set_icon_name: Some("view-refresh-symbolic"),
                                set_title: "No runs yet",
                                set_description: Some(
                                    "Workflow runs in your watched repositories will appear here.",
                                ),
                            },

                            add_named[Some("runs")] = &gtk::ScrolledWindow {
                                set_vexpand: true,

                                adw::Clamp {
                                    set_maximum_size: 700,
                                    set_tightening_threshold: 500,
                                    set_margin_all: 12,

                                    #[local_ref]
                                    runs_group -> adw::PreferencesGroup {},
                                },
                            },

                            #[watch]
                            set_visible_child_name: model.app_page(),
                        },

                        add_named[Some("disconnected")] = &adw::StatusPage {
                            set_icon_name: Some("network-offline-symbolic"),
                            set_title: "Can't connect to GitHub",
                            #[watch]
                            set_description: Some(&model.disconnected_reason()),

                            #[wrap(Some)]
                            set_child = &gtk::Box {
                                set_halign: gtk::Align::Center,

                                #[name = "retry_button"]
                                gtk::Button {
                                    set_label: "Try Again",
                                    add_css_class: "pill",
                                },
                            },
                        },

                        #[watch]
                        set_visible_child_name: model.state_page(),
                    },
                },
            },
        }
    }

    fn init(
        settings: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        // Built up front so its handle can live in the model and be toggled; the
        // menu item that names it is `hidden-when="action-disabled"`, so starting
        // disabled means Sign Out starts hidden.
        let signout_action: RelmAction<SignOutAction> = {
            let sender = sender.input_sender().clone();
            RelmAction::new_stateless(move |_| {
                sender.send(AppMsg::SignOut).ok();
            })
        };
        signout_action.set_enabled(false);
        let editwatch_action: RelmAction<EditWatchlistAction> = {
            let sender = sender.input_sender().clone();
            RelmAction::new_stateless(move |_| {
                sender.send(AppMsg::EditWatchlist).ok();
            })
        };
        editwatch_action.set_enabled(false);
        let refresh_action: RelmAction<RefreshAction> = {
            let sender = sender.input_sender().clone();
            RelmAction::new_stateless(move |_| {
                sender.send(AppMsg::ManualRefresh).ok();
            })
        };
        refresh_action.set_enabled(false);

        // The run rows live directly on the model (no `run_list.rs` wrapper —
        // it'd be one field's worth of indirection). Rows emit an intent; the
        // reducer decides.
        let runs = FactoryVecDeque::builder()
            .launch(adw::PreferencesGroup::default())
            .forward(sender.input_sender(), |output| match output {
                RunRowOutput::OpenInBrowser(url) => AppMsg::OpenInBrowser(url),
            });

        let model = AppModel {
            connection: None,
            state: ViewState::Loading,
            watched: settings.watched.clone(),
            device: None,
            auth_gen: 0,
            toast_overlay: adw::ToastOverlay::new(),
            onboarding: None,
            repo_picker: None,
            runs,
            all_runs: Vec::new(),
            runs_loaded: false,
            refreshing: false,
            poll: None,
            signout_action: signout_action.gio_action().clone(),
            editwatch_action: editwatch_action.gio_action().clone(),
            refresh_action: refresh_action.gio_action().clone(),
            settings,
        };

        let toast_overlay = model.toast_overlay.clone();
        let runs_group = model.runs.widget();

        let widgets = view_output!();

        // The primary menu, built by hand so Sign Out can carry the `hidden-when`
        // attribute the `menu!` macro doesn't expose.
        let menu = gio::Menu::new();
        let refresh_section = gio::Menu::new();
        let refresh_item = gio::MenuItem::new(Some("Refresh"), Some("win.refresh"));
        refresh_item.set_attribute_value("hidden-when", Some(&"action-disabled".to_variant()));
        refresh_section.append_item(&refresh_item);
        menu.append_section(None, &refresh_section);
        let account = gio::Menu::new();
        let edit_item = gio::MenuItem::new(Some("Edit Watchlist"), Some("win.edit-watchlist"));
        edit_item.set_attribute_value("hidden-when", Some(&"action-disabled".to_variant()));
        account.append_item(&edit_item);
        let signout_item = gio::MenuItem::new(Some("Sign Out"), Some("win.sign-out"));
        signout_item.set_attribute_value("hidden-when", Some(&"action-disabled".to_variant()));
        account.append_item(&signout_item);
        menu.append_section(None, &account);
        let app_section = gio::Menu::new();
        app_section.append(Some("About Pitwall"), Some("win.about"));
        app_section.append(Some("Quit"), Some("win.quit"));
        menu.append_section(None, &app_section);
        widgets.menu_button.set_menu_model(Some(&menu));

        let retry_sender = sender.input_sender().clone();
        widgets.retry_button.connect_clicked(move |_| {
            retry_sender.send(AppMsg::Retry).ok();
        });

        let watchlist_sender = sender.input_sender().clone();
        widgets.watchlist_button.connect_clicked(move |_| {
            watchlist_sender.send(AppMsg::EditWatchlist).ok();
        });

        // Menu actions — thin bridges to `AppMsg` (Quit acts directly).
        let about_sender = sender.input_sender().clone();
        let about_action: RelmAction<AboutAction> = RelmAction::new_stateless(move |_| {
            about_sender.send(AppMsg::ShowAbout).ok();
        });
        let quit_action: RelmAction<QuitAction> = RelmAction::new_stateless(|_| {
            relm4::main_application().quit();
        });

        let mut group = RelmActionGroup::<AppMenuActionGroup>::new();
        group.add_action(signout_action);
        group.add_action(editwatch_action);
        group.add_action(refresh_action);
        group.add_action(about_action);
        group.add_action(quit_action);
        group.register_for_widget(&root);

        let app = relm4::main_application();
        app.set_accelerators_for_action::<QuitAction>(&["<primary>q"]);
        app.set_accelerators_for_action::<RefreshAction>(&["<primary>r", "F5"]);

        // Let GTK tell us when the window isn't worth polling for — minimised,
        // fully obscured, or on another workspace all count as "suspended".
        // Wired here (not once connected) because a reconnect runs this path
        // again; `start_poll` already no-ops while disconnected.
        let suspended = sender.input_sender().clone();
        root.connect_suspended_notify(move |window| {
            suspended
                .send(AppMsg::SuspendedChanged(window.is_suspended()))
                .ok();
        });

        // Reading the keyring and connecting both touch I/O, so they can't happen
        // inline in `init` — kick them off as a command.
        Self::dispatch_boot(&sender);

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>, root: &Self::Root) {
        match msg {
            AppMsg::SignIn => {
                // A new sign-in supersedes any in-flight one.
                self.auth_gen += 1;
                let generation = self.auth_gen;
                if let Some(onboarding) = &self.onboarding {
                    onboarding.code_label.set_text("…");
                    onboarding.stack.set_visible_child_name("awaiting");
                }
                // The whole device flow runs in one streaming command: get the
                // code, push it to the UI, then poll until the user authorises.
                // `drop_on_shutdown` cancels the poll if the app closes mid-flow.
                sender.command(move |out, shutdown| {
                    shutdown
                        .register(async move {
                            match client::start_device_flow().await {
                                Ok(flow) => {
                                    out.send(CommandMsg::DeviceCode {
                                        generation,
                                        user_code: flow.user_code.clone(),
                                        verification_uri: flow.verification_uri.clone(),
                                    })
                                    .ok();
                                    let result = match flow.poll().await {
                                        Ok(token) => {
                                            if let Err(err) = secret::store(&token).await {
                                                tracing::warn!(
                                                    "couldn't save token to keyring: {err:#}"
                                                );
                                            }
                                            client::connect(token).await
                                        }
                                        Err(err) => Err(err),
                                    };
                                    out.send(CommandMsg::DeviceResult {
                                        generation,
                                        result: Box::new(result),
                                    })
                                    .ok();
                                }
                                Err(err) => {
                                    out.send(CommandMsg::DeviceResult {
                                        generation,
                                        result: Box::new(Err(err)),
                                    })
                                    .ok();
                                }
                            }
                        })
                        .drop_on_shutdown()
                        .boxed()
                });
            }

            AppMsg::OpenVerification => {
                if let Some((_, uri)) = &self.device {
                    Self::open_uri(uri, root);
                }
            }

            AppMsg::CopyCode => {
                if let Some((code, _)) = &self.device {
                    root.clipboard().set_text(code);
                }
                if let Some(onboarding) = &self.onboarding {
                    onboarding
                        .toast_overlay
                        .add_toast(adw::Toast::new("Code copied"));
                }
            }

            AppMsg::CancelAuth => {
                // Invalidate the in-flight flow; its late result is ignored by
                // the generation check. The orphaned poll expires on GitHub's side.
                self.auth_gen += 1;
                self.device = None;
                if let Some(onboarding) = &self.onboarding {
                    onboarding.stack.set_visible_child_name("intro");
                }
            }

            AppMsg::SignOut => {
                sender.oneshot_command(async move {
                    if let Err(err) = secret::clear().await {
                        tracing::warn!("couldn't clear token from keyring: {err:#}");
                    }
                    CommandMsg::TokenCleared
                });
            }

            AppMsg::EditWatchlist => {
                let Some(connection) = &self.connection else {
                    return;
                };
                // The picker fetches and lists the repos itself; we hand it the
                // client (a cheap Arc clone) and the current watch set to pre-tick,
                // and forward its Saved output back as a message.
                let controller = RepoPicker::builder()
                    .launch(RepoPickerInit {
                        octocrab: connection.octocrab.clone(),
                        watched: self.watched.clone(),
                    })
                    .forward(sender.input_sender(), |output| match output {
                        RepoPickerOutput::Saved(names) => AppMsg::WatchlistSaved(names),
                    });
                controller.widget().present(Some(root));
                self.repo_picker = Some(controller);
            }

            AppMsg::WatchlistSaved(names) => {
                self.watched.clone_from(&names);
                self.settings.watched = names;
                self.settings.save();
                // The dialog closed itself on Save; drop its controller.
                self.repo_picker = None;
                // The watch set changed: clear the old runs and re-poll from
                // scratch (both no-op if the set is now empty → the no-repos page).
                self.all_runs.clear();
                self.runs.guard().clear();
                self.runs_loaded = false;
                self.stop_poll();
                self.start_poll(&sender);
                self.dispatch_refresh(&sender);
            }

            AppMsg::Refresh => self.dispatch_refresh(&sender),

            AppMsg::ManualRefresh => {
                self.refreshing = true;
                self.dispatch_refresh(&sender);
            }

            AppMsg::OpenInBrowser(url) => Self::open_uri(&url, root),

            AppMsg::SuspendedChanged(suspended) => {
                if suspended {
                    self.stop_poll();
                } else {
                    self.start_poll(&sender);
                    // Whatever we last drew is as stale as the pause was long, so
                    // refresh now rather than waiting for the first tick.
                    if matches!(self.state, ViewState::Ready) {
                        self.dispatch_refresh(&sender);
                    }
                }
            }

            AppMsg::Retry => {
                self.state = ViewState::Loading;
                Self::dispatch_boot(&sender);
            }

            AppMsg::ShowAbout => {
                let about = adw::AboutDialog::builder()
                    .application_name("Pitwall")
                    .application_icon(crate::APP_ID)
                    .version(env!("CARGO_PKG_VERSION"))
                    .developer_name("Miguel Rincon")
                    .comments("Monitor your GitHub Actions runs, natively.")
                    .website("https://github.com/SoftARV/Pitwall")
                    .issue_url("https://github.com/SoftARV/Pitwall/issues")
                    .license_type(gtk::License::Gpl30)
                    .copyright("© 2026 Miguel Rincon")
                    .debug_info(match &self.connection {
                        Some(connection) => format!(
                            "Signed in as {}\nRate limit: {} / {} requests remaining this hour",
                            connection.login, connection.rate.remaining, connection.rate.limit,
                        ),
                        None => "Not connected".to_owned(),
                    })
                    .build();
                about.present(Some(root));
            }
        }
    }

    fn update_cmd(
        &mut self,
        msg: Self::CommandOutput,
        sender: ComponentSender<Self>,
        root: &Self::Root,
    ) {
        match msg {
            CommandMsg::Connected(result) => match *result {
                Ok(connection) => {
                    tracing::debug!(login = %connection.login, "connected");
                    self.go_ready(connection, &sender);
                }
                Err(err) => {
                    if err.is_auth() {
                        // A stored token that's now invalid: onboarding, with the
                        // reason toasted onto the modal.
                        self.connection = None;
                        self.show_onboarding(root, &sender);
                        if let Some(onboarding) = &self.onboarding {
                            onboarding
                                .toast_overlay
                                .add_toast(adw::Toast::new(err.message()));
                        }
                    } else {
                        if let Some(onboarding) = self.onboarding.take() {
                            onboarding.dialog.force_close();
                        }
                        self.connection = None;
                        self.state = ViewState::Disconnected(err.message().to_owned());
                        self.set_signed_in(false);
                    }
                }
            },

            CommandMsg::NoToken => {
                self.show_onboarding(root, &sender);
            }

            CommandMsg::TokenCleared => {
                self.connection = None;
                self.show_onboarding(root, &sender);
            }

            CommandMsg::DeviceCode {
                generation,
                user_code,
                verification_uri,
            } => {
                if generation != self.auth_gen {
                    return;
                }
                if let Some(onboarding) = &self.onboarding {
                    onboarding.code_label.set_text(&user_code);
                    onboarding.stack.set_visible_child_name("awaiting");
                }
                // Open the browser to the device page and copy the code, so the
                // user just enters it and approves.
                root.clipboard().set_text(&user_code);
                Self::open_uri(&verification_uri, root);
                self.device = Some((user_code, verification_uri));
            }

            CommandMsg::DeviceResult { generation, result } => {
                if generation != self.auth_gen {
                    return;
                }
                self.device = None;
                match *result {
                    Ok(connection) => {
                        tracing::debug!(login = %connection.login, "signed in");
                        self.go_ready(connection, &sender);
                    }
                    Err(err) => {
                        // Denied, timed out, or a blip after authorising: back to
                        // the intro, with the reason toasted onto the modal.
                        if let Some(onboarding) = &self.onboarding {
                            onboarding.stack.set_visible_child_name("intro");
                            onboarding
                                .toast_overlay
                                .add_toast(adw::Toast::new(err.message()));
                        }
                    }
                }
            }

            CommandMsg::RunsLoaded(runs) => {
                self.all_runs = runs;
                self.runs_loaded = true;
                self.refreshing = false;
                self.apply_runs();
            }

            CommandMsg::RunsFailed(reason) => {
                self.refreshing = false;
                if self.runs_loaded {
                    // We already have runs; a transient failure shouldn't wipe
                    // them — keep the last set and just say so.
                    self.toast(&format!("Couldn't refresh: {reason}"));
                } else {
                    // Never loaded and every repo failed: treat it as offline.
                    self.stop_poll();
                    self.state = ViewState::Disconnected(reason);
                }
            }
        }
    }
}
