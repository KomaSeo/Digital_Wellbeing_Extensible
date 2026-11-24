//! Responsibility: Name this program's own executables, in one place.
//!
//! The enforcer protects whatever names it is given (`enforcer::Enforcer::new`); the agent,
//! the update guard, the lockdown, the AI focus monitor and the block lists take the names
//! from here, so a renamed or added executable is changed once.

/// The Windows service; it also runs as the per-session agent and as the update guard.
pub const SERVICE_EXE: &str = "digital-wellbeing-service.exe";
/// The Windows service's name (the service manager's and the `SafeBoot` registry key's).
pub const SERVICE_NAME: &str = "DigitalWellbeing";
/// The window the user sees.
pub const FOCUS_EXE: &str = "digital-wellbeing-focus.exe";
/// The MCP server Claude Desktop starts in the user's account.
pub const MCP_EXE: &str = "digital-wellbeing-mcp.exe";
/// The old standalone and admin window (removed in rc.79). An older install may still have a
/// copy, which `install` deletes; until then it is protected like the others.
pub const LEGACY_ADMIN_EXES: [&str; 2] = ["digital-wellbeing.exe", "digital_wellbeing.exe"];

/// Every executable of this program: never closed by any rule, never judged by the AI.
pub const OWN_PROGRAMS: [&str; 5] = [
    SERVICE_EXE,
    FOCUS_EXE,
    MCP_EXE,
    LEGACY_ADMIN_EXES[0],
    LEGACY_ADMIN_EXES[1],
];

/// The programs this program runs in a user's session and that hold files an update replaces.
pub const USER_SESSION_PROGRAMS: [&str; 3] = [FOCUS_EXE, MCP_EXE, LEGACY_ADMIN_EXES[0]];

/// Whether `normalized_name` (lower case, with `.exe`, as `enforcer::normalize_process_name`
/// gives it) is one of this program's executables.
pub fn is_own_program(normalized_name: &str) -> bool {
    OWN_PROGRAMS.contains(&normalized_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_name_is_lower_case_with_exe() {
        for name in OWN_PROGRAMS {
            assert_eq!(name, name.to_lowercase());
            assert!(name.ends_with(".exe"), "{name}");
        }
        assert!(is_own_program(FOCUS_EXE));
        assert!(!is_own_program("steam.exe"));
    }
}
