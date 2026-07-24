// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! The shared status chip: a coloured pill with a dot and a state label.
//!
//! One reusable widget — `StatusChip`, a relm4 `WidgetTemplate` — plus the small
//! mapping from a run's `(RunStatus, Conclusion)` to the chip's text and colour
//! class. The run rows use it now; the detail page will reuse it, so the two
//! can't drift apart.
//!
//! A `WidgetTemplate` rather than a full `Component`: the chip has no state of its
//! own, it's pure structure the parents drive with `#[watch]`, which also lets it
//! embed straight into the factory row without a per-row child controller. The
//! pill shape and per-variant colours are the stylesheet at the bottom
//! (`install_css`); the functions decide the label text and the variant class.
//!
//! This is a CLAUDE.md-sanctioned custom-CSS exception (inherited from Dockyard):
//! libadwaita has no chip/badge widget for a filled, coloured pill. Colours are
//! Adwaita's own named colours, so the chip follows the theme for free.

use relm4::gtk::prelude::*;
use relm4::{WidgetTemplate, gtk, set_global_css};

use crate::github::types::{Conclusion, RunStatus};

/// The chip widget: a `.status-chip` pill wrapping a coloured dot and a label.
/// The caller sets the variant class and the label text via `#[watch]`; the dot
/// picks up the matching colour through `.status-chip.<variant> .status-dot` in
/// the stylesheet, so it always tracks the text colour.
#[relm4::widget_template(pub)]
impl WidgetTemplate for StatusChip {
    view! {
        gtk::Box {
            add_css_class: "status-chip",
            set_valign: gtk::Align::Center,
            set_spacing: 6,

            // No name: parents never touch the dot — its colour comes from the
            // `.status-chip.<variant> .status-dot` CSS, keyed off the variant
            // class the parent sets on the root.
            gtk::Box {
                add_css_class: "status-dot",
                set_valign: gtk::Align::Center,
            },

            #[name = "label"]
            gtk::Label {},
        }
    }
}

/// The chip's human-readable label for a run's status/conclusion.
pub fn label(status: RunStatus, conclusion: Option<Conclusion>) -> &'static str {
    match status {
        RunStatus::Queued => "Queued",
        RunStatus::InProgress => "Running",
        RunStatus::Unknown => "Unknown",
        RunStatus::Completed => match conclusion {
            Some(Conclusion::Success) => "Success",
            Some(Conclusion::Failure) => "Failure",
            Some(Conclusion::Cancelled) => "Cancelled",
            Some(Conclusion::Skipped) => "Skipped",
            Some(Conclusion::TimedOut) => "Timed out",
            Some(Conclusion::ActionRequired) => "Action required",
            Some(Conclusion::Neutral) => "Neutral",
            Some(Conclusion::StartupFailure) => "Startup failure",
            Some(Conclusion::Stale) => "Stale",
            Some(Conclusion::Unknown) | None => "Completed",
        },
    }
}

/// The chip's colour-variant class (paired with `status-chip`): green success,
/// red failure, blue in-progress, amber queued/attention, neutral for the rest.
pub fn variant(status: RunStatus, conclusion: Option<Conclusion>) -> &'static str {
    match status {
        RunStatus::Queued => "warning",
        RunStatus::InProgress => "accent",
        RunStatus::Unknown => "neutral",
        RunStatus::Completed => match conclusion {
            Some(Conclusion::Success) => "success",
            Some(Conclusion::Failure | Conclusion::TimedOut | Conclusion::StartupFailure) => {
                "error"
            }
            Some(Conclusion::ActionRequired) => "warning",
            // Cancelled, Skipped, Neutral, Stale, Unknown, or still-null.
            _ => "neutral",
        },
    }
}

/// Build a chip widget imperatively — for lists assembled outside `view!`, like
/// the detail page's jobs and steps. Same structure and classes as the template.
pub fn build(status: RunStatus, conclusion: Option<Conclusion>) -> gtk::Box {
    let chip = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    chip.set_valign(gtk::Align::Center);
    chip.set_css_classes(&["status-chip", variant(status, conclusion)]);

    let dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    dot.add_css_class("status-dot");
    dot.set_valign(gtk::Align::Center);
    chip.append(&dot);

    chip.append(&gtk::Label::new(Some(label(status, conclusion))));
    chip
}

/// Install the chip's stylesheet. Global, once, after GTK is initialised — call
/// from `main` at startup. It has to be global: the variant and dot selectors
/// depend on classes set at runtime, which per-widget CSS can't express.
pub fn install_css() {
    set_global_css(CSS);
}

const CSS: &str = "
.status-chip {
    border-radius: 9999px;
    padding: 3px 10px;
    font-weight: bold;
    font-size: 0.8em;
}
.status-dot {
    min-width: 7px;
    min-height: 7px;
    border-radius: 9999px;
}
/* Tonal: a soft tint of the state colour behind matching text. The `@*_color`
   names are Adwaita's standalone semantic colours, so they track the theme. */
.status-chip.success { background-color: alpha(@success_color, 0.15); color: @success_color; }
.status-chip.error   { background-color: alpha(@error_color, 0.15);   color: @error_color; }
.status-chip.accent  { background-color: alpha(@accent_color, 0.15);  color: @accent_color; }
.status-chip.warning { background-color: alpha(@warning_color, 0.15); color: @warning_color; }
.status-chip.neutral {
    background-color: alpha(@window_fg_color, 0.08);
    color: alpha(@window_fg_color, 0.7);
}
.status-chip.success .status-dot { background-color: @success_color; }
.status-chip.error   .status-dot { background-color: @error_color; }
.status-chip.accent  .status-dot { background-color: @accent_color; }
.status-chip.warning .status-dot { background-color: @warning_color; }
.status-chip.neutral .status-dot { background-color: alpha(@window_fg_color, 0.55); }
";
