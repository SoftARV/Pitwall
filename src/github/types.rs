// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Our own GitHub types — the boundary octocrab's models stop at (CLAUDE.md
//! rule 4).
//!
//! Mapping octocrab's models into ours here keeps the components and the `view!`
//! macros free of octocrab entirely. So far: the rate-limit snapshot and `Repo`;
//! the `WorkflowRun` / `Job` / `Step` types arrive with the run list.

use octocrab::models::Repository;

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

/// A repository we can watch. octocrab's `Repository` — a swamp of optionals — is
/// narrowed to this at the boundary; everything downstream keys off `owner/name`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    pub owner: String,
    pub name: String,
    pub private: bool,
}

impl Repo {
    /// `owner/name` — GitHub's canonical repo identifier, and how the watch list
    /// is stored (in settings) and keyed (in the picker).
    pub fn full_name(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }

    /// Narrow octocrab's model. Returns `None` for a repo with no owner — it's
    /// unusable, since every API call keys off `owner/name`.
    pub fn from_model(repo: Repository) -> Option<Self> {
        Some(Self {
            owner: repo.owner?.login,
            name: repo.name,
            // The API omits `private` for some responses; absent means public.
            private: repo.private.unwrap_or(false),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_name_is_owner_slash_name() {
        let repo = Repo {
            owner: "SoftARV".to_owned(),
            name: "Pitwall".to_owned(),
            private: true,
        };
        assert_eq!(repo.full_name(), "SoftARV/Pitwall");
    }
}
