// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Root component.
//!
//! Redux with a compiler: `AppMsg` are the actions, `update` is the sole
//! reducer, and the view is derived from `AppModel`. Nothing here does I/O
//! inline — every keyring and GitHub call is dispatched as a relm4 `Command` so
//! the GTK main thread never blocks (CLAUDE.md rules 4 and 5).
//!
//! This milestone (M1, PR 1) is the auth slice: cold start reconnects from a
//! saved OAuth token in the keyring, or shows "Sign in with GitHub" → the OAuth
//! **device flow** (a user code + browser authorisation, no client secret) →
//! store the token → verified `Ready`. The repo picker and run list build on top
//! of the `Ready` state this ends at.

use futures_util::FutureExt;
use relm4::actions::{AccelsPlus, RelmAction, RelmActionGroup};
use relm4::adw::prelude::*;
use relm4::gtk::gio;
use relm4::{Component, ComponentParts, ComponentSender, adw, gtk};

use crate::github::client::{self, ConnectError, Connection};
use crate::secret;
use crate::settings::Settings;

// The primary menu's action group. GTK menu items invoke `GAction`s by name;
// this defines the "win" group and the stateless actions in it (fully qualified,
// e.g. `win.about`). The group is registered on the window in `init`, where each
// action bridges to an `AppMsg` (except Quit, which acts directly).
relm4::new_action_group!(AppMenuActionGroup, "win");
relm4::new_stateless_action!(SignOutAction, AppMenuActionGroup, "sign-out");
relm4::new_stateless_action!(AboutAction, AppMenuActionGroup, "about");
relm4::new_stateless_action!(QuitAction, AppMenuActionGroup, "quit");

/// The screen the app is showing. One `gtk::Stack` page each.
#[derive(Debug)]
pub enum ViewState {
    /// Verifying a saved token at startup (or briefly, just after "Sign in"
    /// before the device code arrives).
    Loading,
    /// No usable token — show the "Sign in with GitHub" screen.
    NeedsAuth,
    /// The device flow is live: show the user code and wait for the browser
    /// authorisation.
    AwaitingAuth {
        user_code: String,
        verification_uri: String,
    },
    /// Connected and verified. The repo picker / run list land here next.
    Ready,
    /// Couldn't connect for a non-auth reason (offline, 403, …). Carries the
    /// message, which names the fix.
    Disconnected(String),
}

pub struct AppModel {
    /// The live, verified connection, or `None` when not connected. Holds the
    /// octocrab client every future request goes through.
    connection: Option<Connection>,
    state: ViewState,
    /// Held so `update` can raise toasts. A refcounted GTK handle, not shared
    /// model state — cloning it is a pointer bump; it's the standard relm4
    /// escape hatch for a widget that's commanded rather than declared.
    toast_overlay: adw::ToastOverlay,
    /// Persisted global settings. Loaded before the app ran and handed in.
    #[allow(dead_code)]
    settings: Settings,
}

#[derive(Debug, Clone)]
pub enum AppMsg {
    /// "Sign in with GitHub" — start the device flow.
    SignIn,
    /// Re-open github.com/login/device in the browser.
    OpenVerification,
    /// Copy the current user code to the clipboard.
    CopyCode,
    /// Abandon the in-progress sign-in and return to the start screen.
    CancelAuth,
    /// "Sign Out" — forget the token and return to the sign-in screen.
    SignOut,
    /// "Try Again" on the disconnected page — re-run the startup connect.
    Retry,
    /// Open the About dialog (primary menu).
    ShowAbout,
}

/// Results coming back from commands — off-thread work landing on the main
/// thread. Separate from `AppMsg` because relm4 gives commands their own
/// channel (this is the `CommandOutput` associated type).
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
        user_code: String,
        verification_uri: String,
    },
    /// Device flow finished: authorised (and verified) or failed.
    DeviceResult(Box<Result<Connection, ConnectError>>),
}

impl AppModel {
    fn toast(&self, message: &str) {
        self.toast_overlay.add_toast(adw::Toast::new(message));
    }

    /// Which `Stack` page to show for the current state.
    fn state_page(&self) -> &'static str {
        match self.state {
            ViewState::Loading => "loading",
            ViewState::NeedsAuth => "needs-auth",
            ViewState::AwaitingAuth { .. } => "awaiting-auth",
            ViewState::Ready => "ready",
            ViewState::Disconnected(_) => "disconnected",
        }
    }

    /// The header subtitle: who we're signed in as, once connected. Empty
    /// otherwise, so `adw::WindowTitle` hides the subtitle line.
    fn header_subtitle(&self) -> String {
        match &self.connection {
            Some(connection) => format!("Signed in as {}", connection.login),
            None => String::new(),
        }
    }

    /// The "Ready" page's description: the login plus the remaining rate budget.
    fn ready_description(&self) -> String {
        match &self.connection {
            Some(connection) => format!(
                "Signed in as {}. {} of {} API requests remaining this hour.",
                connection.login, connection.rate.remaining, connection.rate.limit,
            ),
            None => String::new(),
        }
    }

    /// The user code to display while awaiting authorisation.
    fn device_user_code(&self) -> String {
        match &self.state {
            ViewState::AwaitingAuth { user_code, .. } => user_code.clone(),
            _ => String::new(),
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
    /// with the result — opening a browser either works or the user notices.
    fn open_uri(uri: &str, root: &adw::ApplicationWindow) {
        gtk::UriLauncher::new(uri).launch(Some(root), gio::Cancellable::NONE, |_result| {});
    }

    /// Load a saved token off-thread and, if there is one, build and verify a
    /// client from it. Associated rather than a method because `init` and
    /// `Retry` both need it, and `init` has no model to call it on yet.
    fn dispatch_boot(sender: &ComponentSender<Self>) {
        sender.oneshot_command(async move {
            match secret::load().await {
                Ok(Some(token)) => CommandMsg::Connected(Box::new(client::connect(token).await)),
                Ok(None) => CommandMsg::NoToken,
                // The keyring itself failed (no Secret Service running, locked,
                // …). Not an auth problem — treat it as disconnected with the
                // reason, so the user can retry once it's available.
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
            // Opens wide: the run list and detail page (later milestones) want
            // the room, and it's the size the app should feel like from the off.
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

                        // The primary menu — the GNOME hamburger. A real
                        // `gio::Menu` model, so its items invoke `GAction`s and
                        // get keyboard/screen-reader behaviour for free.
                        pack_end = &gtk::MenuButton {
                            set_icon_name: "open-menu-symbolic",
                            set_tooltip_text: Some("Main Menu"),
                            set_menu_model: Some(&primary_menu),
                        },
                    },

                    // One page per state. A `gtk::Stack` (not an `if`/`match`)
                    // so the interactive widgets are built once and can be wired
                    // up in `init`, and so switching states never re-parents
                    // anything.
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

                        add_named[Some("needs-auth")] = &adw::StatusPage {
                            set_icon_name: Some("dialog-password-symbolic"),
                            set_title: "Connect to GitHub",
                            set_description: Some(
                                "Sign in to start monitoring your workflow runs. Pitwall opens \
                                 GitHub in your browser to authorise read access to your \
                                 repositories’ Actions.",
                            ),

                            #[wrap(Some)]
                            set_child = &gtk::Box {
                                set_halign: gtk::Align::Center,

                                #[name = "signin_button"]
                                gtk::Button {
                                    set_label: "Sign in with GitHub",
                                    add_css_class: "suggested-action",
                                    add_css_class: "pill",
                                },
                            },
                        },

                        add_named[Some("awaiting-auth")] = &adw::StatusPage {
                            set_icon_name: Some("dialog-password-symbolic"),
                            set_title: "Authorize Pitwall",
                            set_description: Some(
                                "We opened github.com/login/device in your browser and copied \
                                 your code. Enter it there and approve access.",
                            ),

                            #[wrap(Some)]
                            set_child = &gtk::Box {
                                set_orientation: gtk::Orientation::Vertical,
                                set_spacing: 18,
                                set_halign: gtk::Align::Center,

                                // The user code, big and selectable.
                                gtk::Label {
                                    #[watch]
                                    set_label: &model.device_user_code(),
                                    add_css_class: "title-1",
                                    add_css_class: "numeric",
                                    set_selectable: true,
                                },

                                gtk::Box {
                                    set_spacing: 6,
                                    set_halign: gtk::Align::Center,

                                    #[name = "open_button"]
                                    gtk::Button {
                                        set_label: "Open GitHub",
                                        add_css_class: "pill",
                                    },
                                    #[name = "copy_button"]
                                    gtk::Button {
                                        set_label: "Copy Code",
                                        add_css_class: "pill",
                                    },
                                },

                                gtk::Box {
                                    set_spacing: 8,
                                    set_halign: gtk::Align::Center,
                                    gtk::Spinner { set_spinning: true },
                                    gtk::Label {
                                        set_label: "Waiting for authorization…",
                                        add_css_class: "dim-label",
                                    },
                                },

                                #[name = "cancel_button"]
                                gtk::Button {
                                    set_label: "Cancel",
                                    set_halign: gtk::Align::Center,
                                    add_css_class: "flat",
                                },
                            },
                        },

                        add_named[Some("ready")] = &adw::StatusPage {
                            set_icon_name: Some("emblem-ok-symbolic"),
                            set_title: "Connected to GitHub",
                            #[watch]
                            set_description: Some(&model.ready_description()),
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

                        // Set after the children exist — naming a missing child
                        // is a GTK-CRITICAL.
                        #[watch]
                        set_visible_child_name: model.state_page(),
                    },
                },
            },
        }
    }

    // The primary menu's model. Sibling of `view!` (the component macro wires
    // `primary_menu` into the tree above). Each item names a `GAction`, resolved
    // against the "win" group registered on the window in `init`.
    menu! {
        primary_menu: {
            section! {
                "Sign Out" => SignOutAction,
            },
            section! {
                "About Pitwall" => AboutAction,
                "Quit" => QuitAction,
            }
        }
    }

    fn init(
        settings: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let model = AppModel {
            connection: None,
            state: ViewState::Loading,
            toast_overlay: adw::ToastOverlay::new(),
            settings,
        };

        let toast_overlay = model.toast_overlay.clone();

        let widgets = view_output!();

        // Wire the buttons, here rather than in `view!` because each just posts a
        // fixed message; the reducer reads the current state to act. `bridge`
        // makes a click-to-message closure without repeating the boilerplate.
        let bridge = |msg: AppMsg| {
            let input = sender.input_sender().clone();
            move |_: &gtk::Button| {
                input.send(msg.clone()).ok();
            }
        };
        widgets
            .signin_button
            .connect_clicked(bridge(AppMsg::SignIn));
        widgets
            .open_button
            .connect_clicked(bridge(AppMsg::OpenVerification));
        widgets
            .copy_button
            .connect_clicked(bridge(AppMsg::CopyCode));
        widgets
            .cancel_button
            .connect_clicked(bridge(AppMsg::CancelAuth));
        widgets.retry_button.connect_clicked(bridge(AppMsg::Retry));

        // Menu actions. Each is a thin bridge that posts an `AppMsg`, so the
        // work still lands in the one reducer — except Quit, which acts directly.
        let signout_sender = sender.input_sender().clone();
        let signout_action: RelmAction<SignOutAction> = RelmAction::new_stateless(move |_| {
            signout_sender.send(AppMsg::SignOut).ok();
        });
        let about_sender = sender.input_sender().clone();
        let about_action: RelmAction<AboutAction> = RelmAction::new_stateless(move |_| {
            about_sender.send(AppMsg::ShowAbout).ok();
        });
        let quit_action: RelmAction<QuitAction> = RelmAction::new_stateless(|_| {
            relm4::main_application().quit();
        });

        let mut group = RelmActionGroup::<AppMenuActionGroup>::new();
        group.add_action(signout_action);
        group.add_action(about_action);
        group.add_action(quit_action);
        group.register_for_widget(&root);

        relm4::main_application().set_accelerators_for_action::<QuitAction>(&["<primary>q"]);

        // Reading the keyring and connecting both touch I/O, so they can't happen
        // inline in `init` — kick them off as a command.
        Self::dispatch_boot(&sender);

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>, root: &Self::Root) {
        match msg {
            AppMsg::SignIn => {
                self.state = ViewState::Loading;
                // The whole device flow runs in one streaming command: get the
                // code, push it to the UI, then poll until the user authorises.
                // `drop_on_shutdown` cancels the poll if the app closes mid-flow.
                sender.command(move |out, shutdown| {
                    shutdown
                        .register(async move {
                            match client::start_device_flow().await {
                                Ok(flow) => {
                                    out.send(CommandMsg::DeviceCode {
                                        user_code: flow.user_code.clone(),
                                        verification_uri: flow.verification_uri.clone(),
                                    })
                                    .ok();
                                    let result = match flow.poll().await {
                                        Ok(token) => {
                                            // Store only a token that then
                                            // verifies (below); a store failure
                                            // is rare and non-fatal.
                                            if let Err(err) = secret::store(&token).await {
                                                tracing::warn!(
                                                    "couldn't save token to keyring: {err:#}"
                                                );
                                            }
                                            client::connect(token).await
                                        }
                                        Err(err) => Err(err),
                                    };
                                    out.send(CommandMsg::DeviceResult(Box::new(result))).ok();
                                }
                                Err(err) => {
                                    out.send(CommandMsg::DeviceResult(Box::new(Err(err)))).ok();
                                }
                            }
                        })
                        .drop_on_shutdown()
                        .boxed()
                });
            }

            AppMsg::OpenVerification => {
                if let ViewState::AwaitingAuth {
                    verification_uri, ..
                } = &self.state
                {
                    Self::open_uri(&verification_uri.clone(), root);
                }
            }

            AppMsg::CopyCode => {
                if let ViewState::AwaitingAuth { user_code, .. } = &self.state {
                    root.clipboard().set_text(user_code);
                    self.toast("Code copied");
                }
            }

            AppMsg::CancelAuth => {
                // The background poll for this code is now orphaned; it's ignored
                // on arrival (the guards in `update_cmd` only accept a result
                // while we're still awaiting) and expires on GitHub's side.
                self.state = ViewState::NeedsAuth;
            }

            AppMsg::SignOut => {
                sender.oneshot_command(async move {
                    if let Err(err) = secret::clear().await {
                        tracing::warn!("couldn't clear token from keyring: {err:#}");
                    }
                    CommandMsg::TokenCleared
                });
            }

            AppMsg::Retry => {
                self.state = ViewState::Loading;
                Self::dispatch_boot(&sender);
            }

            AppMsg::ShowAbout => {
                // A standard adw::AboutDialog from our own metadata. `Gpl30` is
                // GTK's name for "v3 or later"; it renders the full notice so we
                // don't hand-write it. The icon resolves to the installed themed
                // icon (a generic fallback before `make install`).
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
                    .build();
                about.present(Some(root));
            }
        }
    }

    fn update_cmd(
        &mut self,
        msg: Self::CommandOutput,
        _sender: ComponentSender<Self>,
        root: &Self::Root,
    ) {
        match msg {
            CommandMsg::Connected(result) => match *result {
                Ok(connection) => {
                    tracing::debug!(
                        login = %connection.login,
                        remaining = connection.rate.remaining,
                        "connected",
                    );
                    self.connection = Some(connection);
                    self.state = ViewState::Ready;
                    // (Run-list milestone: start the poll here.)
                }
                Err(err) => {
                    if err.is_auth() {
                        // A stored token that's now invalid: back to sign-in, with
                        // the reason as a toast so it isn't silently swallowed.
                        self.connection = None;
                        self.toast(err.message());
                        self.state = ViewState::NeedsAuth;
                    } else {
                        self.state = ViewState::Disconnected(err.message().to_owned());
                    }
                }
            },

            CommandMsg::NoToken => {
                self.state = ViewState::NeedsAuth;
            }

            CommandMsg::TokenCleared => {
                self.connection = None;
                self.state = ViewState::NeedsAuth;
            }

            CommandMsg::DeviceCode {
                user_code,
                verification_uri,
            } => {
                // Ignore a code from a sign-in the user has since cancelled.
                if !matches!(
                    self.state,
                    ViewState::Loading | ViewState::AwaitingAuth { .. }
                ) {
                    return;
                }
                // Open the browser to the device page and copy the code, so the
                // user just enters it and approves.
                root.clipboard().set_text(&user_code);
                Self::open_uri(&verification_uri, root);
                self.state = ViewState::AwaitingAuth {
                    user_code,
                    verification_uri,
                };
            }

            CommandMsg::DeviceResult(result) => {
                // Only act on a result for the sign-in we're still awaiting — an
                // orphaned (cancelled) flow's late result is dropped here.
                if !matches!(self.state, ViewState::AwaitingAuth { .. }) {
                    return;
                }
                match *result {
                    Ok(connection) => {
                        tracing::debug!(login = %connection.login, "signed in");
                        self.connection = Some(connection);
                        self.state = ViewState::Ready;
                    }
                    Err(err) => {
                        // Denied, timed out, or a network blip after authorising:
                        // back to the sign-in screen with the reason.
                        self.toast(err.message());
                        self.state = ViewState::NeedsAuth;
                    }
                }
            }
        }
    }
}
