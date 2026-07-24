// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Desktop notifications — the app's headline feature.
//!
//! When a watched run flips to *completed* and its outcome matches the user's
//! `NotifyOn` setting, we raise a native `gio::Notification` through the
//! application (CLAUDE.md M6). Native means GNOME renders it with the app's icon,
//! it lands in the notification tray, and a click can reach back into the app.
//!
//! The *transition* detection (was-running → now-finished) lives in `app`, which
//! owns the `last_conclusion` map; this module only builds and sends the alert
//! once `app` has decided one is due.

use relm4::gtk::gio;
use relm4::gtk::prelude::*;
use relm4::main_application;

use crate::github::types::{Conclusion, WorkflowRun};

/// The `app.`-scoped action a notification click activates. The app registers it
/// (see `app`'s `init`); its string target is the run's `html_url`. It must be
/// app-scoped, not `win.`, because GNOME activates a notification's action on the
/// *application*, which may be reached while the window is in the background.
pub const OPEN_RUN_ACTION: &str = "app.open-run";

/// Raise a desktop notification for `run`, which has just finished as
/// `conclusion`. Keyed by run id, so a later notification for the same run
/// replaces its banner rather than stacking a duplicate.
pub fn notify_finished(run: &WorkflowRun, conclusion: Conclusion) {
    let title = if conclusion.is_failure() {
        "Run failed"
    } else if conclusion == Conclusion::Cancelled {
        "Run cancelled"
    } else if conclusion == Conclusion::Success {
        "Run passed"
    } else {
        "Run finished"
    };
    let body = format!("{} · {} · {}", run.workflow_name, run.repo, run.head_branch);

    let notification = gio::Notification::new(title);
    notification.set_body(Some(&body));
    // A failure wants attention now; a pass can be quiet.
    notification.set_priority(if conclusion.is_failure() {
        gio::NotificationPriority::High
    } else {
        gio::NotificationPriority::Normal
    });
    // Clicking the notification opens the run on github.com via the registered
    // action; the URL rides along as the action's target.
    notification
        .set_default_action_and_target_value(OPEN_RUN_ACTION, Some(&run.html_url.to_variant()));

    // If nothing appears despite this line logging, the desktop lacks a matching
    // installed `.desktop` (the GNOME "gtk" notification backend drops app-less
    // notifications) — install `data/dev.miguelrincon.Pitwall.desktop`.
    tracing::debug!(id = run.id, title, "sending desktop notification");
    main_application().send_notification(Some(&notification_id(run.id)), &notification);
}

/// A stable per-run id, so re-notifying a run coalesces onto one banner.
fn notification_id(run_id: u64) -> String {
    format!("run-{run_id}")
}
