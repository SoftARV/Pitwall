// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Our own GitHub types — the boundary octocrab's models stop at (CLAUDE.md
//! rule 4).
//!
//! For M1 this is just the rate-limit snapshot; the `WorkflowRun` / `Job` /
//! `Step` / `Repo` types (with their tests) arrive with the run list. Mapping
//! octocrab's models into ours here keeps the components and the `view!` macros
//! free of octocrab entirely.

/// A snapshot of the authenticated **core** rate-limit budget.
///
/// Authenticated REST gives 5000 requests/hour; this is how much of that is
/// left. Read once at connect time now; from the poll's response headers later,
/// to drive the backoff and the header readout (CLAUDE.md rule 3).
#[derive(Debug, Clone, Copy)]
pub struct RateLimit {
    pub remaining: u64,
    pub limit: u64,
}
