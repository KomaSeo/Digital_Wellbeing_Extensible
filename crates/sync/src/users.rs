//! Responsibility: Keep the names of the owner's logins, shared with every device
//! (`users.json`). Passwords are not here, not even as a hash: the server checks them with
//! OPAQUE ([`crate::account`]) and knows nothing from which a password could be read.
//!
//! A Windows account on a device logs in once (Focus app); from then on its access grants,
//! blocks and AI usage belong to the login, so they are the same on every device whatever
//! the Windows account is called. Only the server writes this document.

use serde::{Deserialize, Serialize};

/// The shared document of logins (written by the server only).
pub const USERS_DOCUMENT: &str = "users.json";

/// The shortest password a login may have: the password is the only thing between someone
/// who has the server's files and the owner's API key, so a short one could be guessed.
pub const MIN_PASSWORD_CHARACTERS: usize = 8;

/// Responsibility: Hold one login's name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserEntry {
    pub name: String,
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

/// A password long enough for a login.
pub fn check_password(password: &str) -> Result<(), String> {
    if password.chars().count() < MIN_PASSWORD_CHARACTERS {
        return Err(format!(
            "비밀번호는 {MIN_PASSWORD_CHARACTERS}자 이상이어야 합니다"
        ));
    }
    Ok(())
}

impl Users {
    /// Add `name` (no change when it is there).
    pub fn add(&mut self, name: &str) -> Result<(), String> {
        let name = clean_name(name)?;
        if !self.contains(&name) {
            self.users.push(UserEntry { name });
            self.users
                .sort_by(|first, second| first.name.cmp(&second.name));
        }
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_cleaned_and_kept_once() {
        let mut users = Users::default();
        users.add(" Me ").unwrap();
        users.add("me").unwrap();
        assert_eq!(users.names(), vec!["me".to_string()]);
        assert!(users.contains("ME"));
        assert!(users.add("../x").is_err());
        assert!(check_password("seven77").is_err());
        assert!(check_password("eight888").is_ok());
        assert!(users.remove("me"));
        assert!(!users.contains("me"));
    }

    #[test]
    fn an_older_users_file_with_hashes_still_reads() {
        let users: Users = serde_json::from_str(
            r#"{"users":[{"name":"me","salt":"x","hash":"y","rounds":200000}]}"#,
        )
        .unwrap();
        assert!(users.contains("me"));
    }
}
