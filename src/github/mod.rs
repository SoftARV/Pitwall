// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! The GitHub boundary: everything that speaks octocrab lives under here.
//!
//! `client` builds and drives the API; `types` holds our own structs that
//! octocrab's response models are mapped into (CLAUDE.md rule 4). Nothing
//! outside this module imports `octocrab`.

pub mod client;
pub mod types;
