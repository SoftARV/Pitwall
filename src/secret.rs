// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! The GitHub token, kept in the system keyring.
//!
//! CLAUDE.md rule 2: the token is a secret, so it lives in the Secret Service
//! (GNOME Keyring) via `oo7`, keyed by our app id — never in the config file,
//! never in a log, never in an error string. These three async wrappers are the
//! only code in the app that touches it.
//!
//! Every call is `async` because the Secret Service is a D-Bus service: talking
//! to it is I/O, so (rule 5) it must run off the GTK main thread, inside a relm4
//! command.

use std::collections::HashMap;

use anyhow::{Context, Result};
use oo7::{Keyring, Secret};

/// The human-readable label GNOME shows in Seahorse ("Passwords and Keys").
const LABEL: &str = "Pitwall GitHub token";

/// The attributes that identify *our* item in the keyring, keyed by the app id
/// so we own exactly one and can't collide with anything else on the machine.
/// `oo7`'s `AsAttributes` is implemented for `HashMap<K, V>` where `K, V: AsRef<str>`,
/// so a map of `&str` is the natural shape.
fn attributes() -> HashMap<&'static str, &'static str> {
    HashMap::from([("application", crate::APP_ID), ("type", "github-token")])
}

/// Store (or replace) the token. `replace = true` means re-pasting a token
/// overwrites the existing item rather than piling up a second one.
pub async fn store(token: &str) -> Result<()> {
    let keyring = Keyring::new().await.context("open keyring")?;
    // The login keyring is normally already unlocked at login, so this is a
    // no-op then and only prompts if it's genuinely locked. Best-effort: a
    // failure here shouldn't stop us attempting the write.
    let _ = keyring.unlock().await;
    keyring
        .create_item(LABEL, &attributes(), Secret::text(token), true)
        .await
        .context("write token to keyring")?;
    Ok(())
}

/// Load the token, if one was stored. `Ok(None)` — not an error — is the
/// first-launch case that sends the app to its token-entry screen.
pub async fn load() -> Result<Option<String>> {
    let keyring = Keyring::new().await.context("open keyring")?;
    let _ = keyring.unlock().await;
    let items = keyring
        .search_items(&attributes())
        .await
        .context("search keyring")?;
    let Some(item) = items.first() else {
        return Ok(None);
    };
    let secret = item.secret().await.context("read token from keyring")?;
    // The secret is raw bytes; our token is UTF-8 text. This conversion is the
    // one place it could fail, and a non-UTF-8 secret isn't a token we wrote.
    let token =
        String::from_utf8(secret.as_bytes().to_vec()).context("stored token is not valid UTF-8")?;
    Ok(Some(token))
}

/// Forget the token — the "log out" path.
pub async fn clear() -> Result<()> {
    let keyring = Keyring::new().await.context("open keyring")?;
    let _ = keyring.unlock().await;
    keyring
        .delete(&attributes())
        .await
        .context("delete token from keyring")?;
    Ok(())
}
