//! Responsibility: Name this computer the same way every time the program is installed on it,
//! so the server gives a reinstalled PC its old device entry back instead of a second one.
//!
//! The owner saw the same computer twice in the device list after installing again ("같은
//! 컴퓨터에 두번 설치하면 서로 다르게 나오더라?"): an uninstall that deletes the user data
//! deletes `sync.json` and with it the device id, so the next login made a new device.
//!
//! - The name comes from what the operating system keeps for the computer and an uninstall
//!   of this program does not touch: on Windows the registry's `MachineGuid`
//!   (`HKLM\SOFTWARE\Microsoft\Cryptography`), elsewhere `/etc/machine-id`. Installing
//!   Windows again makes a new one.
//! - Only a hash of it leaves the PC ([`hash`]), so the server never learns the value itself.
//! - The program that syncs sets it once at start ([`set`]); until then it is `None`, so the
//!   library's tests, which run several "devices" on one computer, are never taken for one.

use sha2::{Digest, Sha256};
use std::sync::RwLock;

static MACHINE_ID: RwLock<Option<String>> = RwLock::new(None);

/// This computer's hashed id, once [`set`] was called with one.
pub fn get() -> Option<String> {
    MACHINE_ID.read().ok().and_then(|slot| slot.clone())
}

/// Use `id` (already hashed) as this computer's id from now on.
pub fn set(id: Option<String>) {
    if let Ok(mut slot) = MACHINE_ID.write() {
        *slot = id;
    }
}

/// The hash sent for a raw machine id: 32 hexadecimal digits of SHA-256, with this program's
/// name mixed in so it matches no other program's use of the same value.
pub fn hash(raw: &str) -> String {
    let digest = Sha256::digest(format!("digital-wellbeing machine:{}", raw.trim()).as_bytes());
    digest
        .iter()
        .take(16)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// This computer's hashed id from the operating system. `DW_MACHINE_ID` replaces it (the
/// tests that run two "PCs" on one computer); an empty value means none.
pub fn from_system() -> Option<String> {
    if let Ok(given) = std::env::var("DW_MACHINE_ID") {
        return (!given.trim().is_empty()).then(|| hash(&given));
    }
    raw_machine_id()
        .filter(|raw| !raw.trim().is_empty())
        .map(|raw| hash(&raw))
}

#[cfg(windows)]
fn raw_machine_id() -> Option<String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let output = std::process::Command::new("reg")
        .args([
            "query",
            r"HKLM\SOFTWARE\Microsoft\Cryptography",
            "/v",
            "MachineGuid",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    // "    MachineGuid    REG_SZ    1234abcd-…"
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find(|line| line.contains("MachineGuid"))
        .and_then(|line| line.split_whitespace().last())
        .map(str::to_string)
}

#[cfg(not(windows))]
fn raw_machine_id() -> Option<String> {
    std::fs::read_to_string("/etc/machine-id").ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hash_is_stable_and_hides_the_value() {
        let first = hash("1234abcd-0000-1111-2222-333344445555");
        assert_eq!(first, hash(" 1234abcd-0000-1111-2222-333344445555\n"));
        assert_eq!(first.len(), 32);
        assert!(!first.contains("1234abcd"));
        assert_ne!(first, hash("another computer"));
    }
}
