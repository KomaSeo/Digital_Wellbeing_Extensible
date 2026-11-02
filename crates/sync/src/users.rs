//! Responsibility: Keep the logins of the owner's devices: a name and a password hash per
//! login, made by the server's administrator and shared with every device (`users.json`).
//!
//! A Windows account on a device logs in once (Focus app); from then on its access grants,
//! blocks and AI usage belong to the login, so they are the same on every device whatever
//! the Windows account is called. Only the server writes this document.

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde::{Deserialize, Serialize};

/// The shared document of logins (written by the server only).
pub const USERS_DOCUMENT: &str = "users.json";
/// PBKDF2-HMAC-SHA256 rounds for a new password (about a tenth of a second).
const ROUNDS: u32 = 200_000;
const SALT_BYTES: usize = 16;

/// Responsibility: Hold one login: its name and how to check its password.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserEntry {
    pub name: String,
    /// Base64.
    pub salt: String,
    /// Base64 of PBKDF2-HMAC-SHA256(password, salt, rounds).
    pub hash: String,
    pub rounds: u32,
}

/// Responsibility: Hold every login (`users.json`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Users {
    pub users: Vec<UserEntry>,
}

/// A login name: letters, digits, `-`, `_` and `.`, 1 to 40 characters, lower case.
pub fn clean_name(name: &str) -> Result<String, String> {
    let name = name.trim().to_lowercase();
    if name.is_empty()
        || name.chars().count() > 40
        || name.starts_with('.')
        || !name
            .chars()
            .all(|character| character.is_alphanumeric() || "-_.".contains(character))
    {
        return Err("a login name is 1 to 40 letters, digits, '-', '_' or '.'".into());
    }
    Ok(name)
}

fn hash(password: &str, salt: &[u8], rounds: u32) -> Vec<u8> {
    let mut output = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<sha2::Sha256>(password.as_bytes(), salt, rounds, &mut output);
    output.to_vec()
}

impl Users {
    /// Add `name` with `password`, or give an existing login a new password.
    pub fn set(&mut self, name: &str, password: &str) -> Result<(), String> {
        let name = clean_name(name)?;
        if password.chars().count() < 4 {
            return Err("the password needs at least 4 characters".into());
        }
        let mut salt = [0u8; SALT_BYTES];
        getrandom::getrandom(&mut salt).map_err(|error| error.to_string())?;
        let entry = UserEntry {
            name: name.clone(),
            salt: STANDARD.encode(salt),
            hash: STANDARD.encode(hash(password, &salt, ROUNDS)),
            rounds: ROUNDS,
        };
        self.users.retain(|user| user.name != name);
        self.users.push(entry);
        self.users
            .sort_by(|first, second| first.name.cmp(&second.name));
        Ok(())
    }

    pub fn remove(&mut self, name: &str) -> bool {
        let name = name.trim().to_lowercase();
        let before = self.users.len();
        self.users.retain(|user| user.name != name);
        self.users.len() != before
    }

    pub fn names(&self) -> Vec<String> {
        self.users.iter().map(|user| user.name.clone()).collect()
    }

    pub fn contains(&self, name: &str) -> bool {
        let name = name.trim().to_lowercase();
        self.users.iter().any(|user| user.name == name)
    }

    /// Is `password` the password of `name`? Returns the login's clean name.
    pub fn check(&self, name: &str, password: &str) -> Option<String> {
        let name = name.trim().to_lowercase();
        let user = self.users.iter().find(|user| user.name == name)?;
        let salt = STANDARD.decode(&user.salt).ok()?;
        let expected = STANDARD.decode(&user.hash).ok()?;
        let actual = hash(password, &salt, user.rounds);
        // The same time whatever byte differs.
        let same = expected.len() == actual.len()
            && expected
                .iter()
                .zip(&actual)
                .fold(0u8, |difference, (first, second)| {
                    difference | (first ^ second)
                })
                == 0;
        same.then(|| user.name.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_right_password_logs_in() {
        let mut users = Users::default();
        users.set(" Me ", "correct horse").unwrap();
        assert_eq!(users.check("me", "correct horse"), Some("me".to_string()));
        assert_eq!(users.check("ME", "correct horse"), Some("me".to_string()));
        assert_eq!(users.check("me", "wrong"), None);
        assert_eq!(users.check("nobody", "correct horse"), None);
        assert!(users.set("../x", "password").is_err());
        assert!(users.set("me", "abc").is_err());
        // A new password replaces the old one.
        users.set("me", "battery staple").unwrap();
        assert_eq!(users.check("me", "correct horse"), None);
        assert_eq!(users.names(), vec!["me".to_string()]);
        assert!(users.remove("me"));
        assert!(!users.contains("me"));
    }
}
