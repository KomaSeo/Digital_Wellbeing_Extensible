//! Responsibility: Keep one Anthropic API key for all of the owner's PCs on the sync server,
//! sealed so that the server (and whoever has its files) cannot read it.
//!
//! - The key is sealed (XChaCha20-Poly1305) with the **vault key**, which each PC gets from
//!   OPAQUE's export key when it logs in ([`crate::account::Joined::vault_key`]). The export
//!   key comes from the password and the server's OPRF key together; the server never sees it.
//! - The sealed key travels as the shared document `vault.json`, like any other document.
//! - Each PC keeps using its own `ai_focus.json`'s `api_key` (the agent reads it there; the
//!   `api_key` value itself is never sent). [`reconcile`] runs before and after every sync:
//!   a key changed on this PC is sealed into `vault.json`, and a key another PC sealed is
//!   written into `ai_focus.json`. When both changed, the server's wins, as in every merge.
//! - A change of the key while the settings are protected goes through the scheduled update
//!   of the PC where it was made (three confirmations), and only then reaches the vault.
//! - `vault_base.json` (this PC only) holds the SHA-256 of the key both last agreed on, to
//!   tell which side changed.

use crate::envelope::{self, Envelope};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::Path;

pub const VAULT_DOCUMENT: &str = "vault.json";
const BASE_FILE: &str = "vault_base.json";
const SETTINGS_FILE: &str = "ai_focus.json";
const ENVELOPE_ID: &str = "vault";

/// Responsibility: Say what [`reconcile`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VaultChange {
    /// This PC's key was sealed into `vault.json` (sent at the next sync).
    Sealed,
    /// The key in `vault.json` was written into this PC's settings.
    TookTheVaultKey,
}

fn key_hash(api_key: &str) -> String {
    Sha256::digest(api_key.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn read(path: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// `api_key`, sealed with `vault_key` (the content of `vault.json`).
pub fn seal(api_key: &str, vault_key: &str) -> Result<Value, String> {
    let sealed = envelope::seal(ENVELOPE_ID, vault_key, api_key.as_bytes())?;
    serde_json::to_value(sealed).map_err(|error| error.to_string())
}

/// The API key in `vault.json`, if `vault_key` opens it.
pub fn open(content: &Value, vault_key: &str) -> Option<String> {
    let sealed: Envelope = serde_json::from_value(content.clone()).ok()?;
    String::from_utf8(envelope::open(&sealed, vault_key).ok()?).ok()
}

/// Make this PC's API key and the vault agree (see the module comment).
pub fn reconcile(data_directory: &Path, vault_key: &str) -> Result<Option<VaultChange>, String> {
    let settings_path = data_directory.join(SETTINGS_FILE);
    let vault_path = data_directory.join(VAULT_DOCUMENT);
    let base_path = data_directory.join(BASE_FILE);
    let settings = read(&settings_path).unwrap_or_else(|| json!({}));
    let local = settings["api_key"].as_str().unwrap_or_default().to_string();
    // A vault sealed under another password (the server was set up again) does not open:
    // as if there were none.
    let vault = read(&vault_path).and_then(|content| open(&content, vault_key));
    let base = read(&base_path).and_then(|value| value["key_sha256"].as_str().map(str::to_string));
    let agree = |api_key: &str| -> Result<(), String> {
        crate::server::write_json(&base_path, &json!({ "key_sha256": key_hash(api_key) }))
    };
    let seal_local = || -> Result<Option<VaultChange>, String> {
        crate::server::write_json(&vault_path, &seal(&local, vault_key)?)?;
        agree(&local)?;
        Ok(Some(VaultChange::Sealed))
    };
    match vault {
        None if local.is_empty() => Ok(None),
        None => seal_local(),
        Some(vault) if vault == local => {
            agree(&local)?;
            Ok(None)
        }
        // Only this PC changed its key.
        Some(vault) if base.as_deref() == Some(key_hash(&vault).as_str()) => seal_local(),
        // The vault changed (or both did, or this PC never agreed with it): the vault's key.
        Some(vault) => {
            let mut settings = settings;
            settings["api_key"] = Value::String(vault.clone());
            crate::server::write_json(&settings_path, &settings)?;
            agree(&vault)?;
            Ok(Some(VaultChange::TookTheVaultKey))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(name: &str) -> std::path::PathBuf {
        let folder = std::env::temp_dir().join(format!("dw_vault_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).unwrap();
        folder
    }

    fn set_key(folder: &Path, api_key: &str) {
        std::fs::write(
            folder.join(SETTINGS_FILE),
            json!({ "api_key": api_key, "model": "m" }).to_string(),
        )
        .unwrap();
    }

    fn key_of(folder: &Path) -> String {
        read(&folder.join(SETTINGS_FILE)).unwrap()["api_key"]
            .as_str()
            .unwrap()
            .to_string()
    }

    #[test]
    fn a_key_goes_into_the_vault_and_comes_out_on_another_pc() {
        let vault_key = envelope::new_key().unwrap();
        let laptop = folder("laptop");
        let desktop = folder("desktop");
        set_key(&laptop, "sk-laptop");
        assert_eq!(
            reconcile(&laptop, &vault_key).unwrap(),
            Some(VaultChange::Sealed)
        );
        let sealed = std::fs::read_to_string(laptop.join(VAULT_DOCUMENT)).unwrap();
        assert!(!sealed.contains("sk-laptop"));

        // The desktop had its own key and never agreed with the vault: the vault's key wins.
        set_key(&desktop, "sk-desktop");
        std::fs::write(desktop.join(VAULT_DOCUMENT), &sealed).unwrap();
        assert_eq!(
            reconcile(&desktop, &vault_key).unwrap(),
            Some(VaultChange::TookTheVaultKey)
        );
        assert_eq!(key_of(&desktop), "sk-laptop");
        // Other settings stay.
        assert_eq!(read(&desktop.join(SETTINGS_FILE)).unwrap()["model"], "m");
        assert_eq!(reconcile(&desktop, &vault_key).unwrap(), None);

        // A new key on the desktop goes into the vault.
        set_key(&desktop, "sk-new");
        assert_eq!(
            reconcile(&desktop, &vault_key).unwrap(),
            Some(VaultChange::Sealed)
        );
        std::fs::copy(desktop.join(VAULT_DOCUMENT), laptop.join(VAULT_DOCUMENT)).unwrap();
        assert_eq!(
            reconcile(&laptop, &vault_key).unwrap(),
            Some(VaultChange::TookTheVaultKey)
        );
        assert_eq!(key_of(&laptop), "sk-new");

        // A vault sealed under another password counts as none.
        let other_key = envelope::new_key().unwrap();
        assert_eq!(
            reconcile(&laptop, &other_key).unwrap(),
            Some(VaultChange::Sealed)
        );
        assert_eq!(key_of(&laptop), "sk-new");
        let _ = std::fs::remove_dir_all(&laptop);
        let _ = std::fs::remove_dir_all(&desktop);
    }
}
