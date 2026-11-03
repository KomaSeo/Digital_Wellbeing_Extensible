//! Responsibility: Keep a password only as a salted hash (PBKDF2-HMAC-SHA256) and check a
//! typed password against it, the same time whatever byte differs.

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde::{Deserialize, Serialize};

/// Rounds for a new password (about a tenth of a second).
const ROUNDS: u32 = 200_000;
const SALT_BYTES: usize = 16;

/// Responsibility: Hold what is needed to check a password, never the password itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PasswordHash {
    /// Base64.
    pub salt: String,
    /// Base64 of PBKDF2-HMAC-SHA256(password, salt, rounds).
    pub hash: String,
    pub rounds: u32,
}

fn derive(password: &str, salt: &[u8], rounds: u32) -> Vec<u8> {
    let mut output = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<sha2::Sha256>(password.as_bytes(), salt, rounds, &mut output);
    output.to_vec()
}

impl PasswordHash {
    pub fn new(password: &str) -> Result<Self, String> {
        let mut salt = [0u8; SALT_BYTES];
        getrandom::getrandom(&mut salt).map_err(|error| error.to_string())?;
        Ok(Self {
            salt: STANDARD.encode(salt),
            hash: STANDARD.encode(derive(password, &salt, ROUNDS)),
            rounds: ROUNDS,
        })
    }

    pub fn matches(&self, password: &str) -> bool {
        let (Ok(salt), Ok(expected)) = (STANDARD.decode(&self.salt), STANDARD.decode(&self.hash))
        else {
            return false;
        };
        let actual = derive(password, &salt, self.rounds);
        expected.len() == actual.len()
            && expected
                .iter()
                .zip(&actual)
                .fold(0u8, |difference, (first, second)| {
                    difference | (first ^ second)
                })
                == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_same_password_matches() {
        let hash = PasswordHash::new("correct horse").unwrap();
        assert!(hash.matches("correct horse"));
        assert!(!hash.matches("correct hors"));
        assert!(!hash.matches(""));
        assert!(!hash.hash.contains("correct"));
        // A new hash of the same password has its own salt.
        assert_ne!(PasswordHash::new("correct horse").unwrap().hash, hash.hash);
    }
}
