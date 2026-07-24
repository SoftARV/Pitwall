// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Reusable UI components. Each is a relm4 `Component` (or, later,
//! `FactoryComponent`); none import `octocrab` — they speak only our own types
//! from `github::types` (CLAUDE.md rule 4).

pub mod repo_picker;
pub mod run_detail;
pub mod run_row;
pub mod status_chip;
