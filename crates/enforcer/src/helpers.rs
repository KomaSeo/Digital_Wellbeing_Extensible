//! Responsibility: Find the helper programs of a blocked program, so they are closed with it.
//!
//! A program often runs helpers under other names (`steam.exe` runs `steamwebhelper.exe`,
//! a launcher runs its updater). Blocking the program by name leaves them running. A helper
//! here is any process whose program file lies in the blocked program's own folder (or a
//! folder inside it). Folders many programs share (`C:\Program Files`, the Windows folder, a
//! user's AppData, `/usr/bin`, …) are never treated as one program's folder, so a program
//! that sits directly in one of them gets no helpers.

use crate::rule::ProcessSnapshot;

/// Lower-case path parts, `\` and `/` alike, without the drive (`C:`).
fn parts(path: &str) -> Vec<String> {
    path.split(['\\', '/'])
        .filter(|part| !part.is_empty())
        .filter(|part| !(part.len() == 2 && part.ends_with(':')))
        .map(|part| part.to_lowercase())
        .collect()
}

/// Is `folder` (as parts) shared by many programs?
fn is_shared_folder(folder: &[String]) -> bool {
    let text: Vec<&str> = folder.iter().map(String::as_str).collect();
    let windows_folder = std::env::var("SystemRoot")
        .map(|root| parts(&root))
        .unwrap_or_else(|_| vec!["windows".to_string()]);
    if folder.starts_with(&windows_folder) {
        return true;
    }
    match text.as_slice() {
        // The drive root and its first level (C:\Program Files, C:\ProgramData, C:\Games).
        [] | [_] => true,
        // A user's folders and their common subfolders.
        ["users", _] | ["home", _] => true,
        ["users", _, "appdata"]
        | ["users", _, "desktop"]
        | ["users", _, "downloads"]
        | ["users", _, "documents"] => true,
        ["users", _, "appdata", "local"]
        | ["users", _, "appdata", "roaming"]
        | ["users", _, "appdata", "locallow"] => true,
        ["users", _, "appdata", "local", "programs"] | ["users", _, "appdata", "local", "temp"] => {
            true
        }
        // Unix program folders (/usr/bin, /usr/local/bin, /usr/lib, …).
        ["usr", ..] if text.len() <= 3 => matches!(
            text.last(),
            Some(
                &"bin" | &"sbin" | &"lib" | &"lib64" | &"libexec" | &"share" | &"games" | &"local"
            )
        ),
        _ => text.last() == Some(&"temp") || text.last() == Some(&"tmp"),
    }
}

/// The program folder of `exe` (its folder), unless that folder is shared.
fn own_folder(exe: &str) -> Option<Vec<String>> {
    let mut folder = parts(exe);
    folder.pop()?;
    (!is_shared_folder(&folder)).then_some(folder)
}

/// Processes of `processes` that are helpers of `blocked` (see the module comment), not
/// `blocked` itself.
pub fn helpers_of<'a>(
    blocked: &ProcessSnapshot,
    processes: &'a [ProcessSnapshot],
) -> Vec<&'a ProcessSnapshot> {
    let Some(folder) = blocked.exe.as_deref().and_then(own_folder) else {
        return Vec::new();
    };
    processes
        .iter()
        .filter(|process| process.pid != blocked.pid)
        .filter(|process| {
            process.exe.as_deref().is_some_and(|exe| {
                let exe_parts = parts(exe);
                exe_parts.len() > folder.len() && exe_parts.starts_with(&folder)
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(pid: u32, exe: &str) -> ProcessSnapshot {
        ProcessSnapshot {
            pid,
            name: exe.rsplit(['\\', '/']).next().unwrap().to_string(),
            exe: Some(exe.to_string()),
        }
    }

    #[test]
    fn helpers_are_the_programs_in_the_blocked_programs_folder() {
        let steam = process(1, r"C:\Program Files (x86)\Steam\steam.exe");
        let all = vec![
            steam.clone(),
            process(
                2,
                r"C:\Program Files (x86)\Steam\bin\cef\cef.win7x64\steamwebhelper.exe",
            ),
            process(
                3,
                r"C:\Program Files (x86)\Steam\steamapps\common\Game\game.exe",
            ),
            process(4, r"C:\Program Files\Google\Chrome\Application\chrome.exe"),
            process(5, r"C:\Program Files (x86)\SteamTools\other.exe"),
        ];
        let found: Vec<u32> = helpers_of(&steam, &all)
            .iter()
            .map(|helper| helper.pid)
            .collect();
        assert_eq!(found, vec![2, 3]);
    }

    #[test]
    fn a_program_in_a_shared_folder_has_no_helpers() {
        for exe in [
            r"C:\Program Files\lonely.exe",
            r"C:\Windows\System32\notepad.exe",
            r"C:\Users\me\Downloads\game.exe",
            r"C:\Users\me\AppData\Local\Programs\x.exe",
            "/usr/bin/game",
            "/usr/local/bin/game",
        ] {
            let blocked = process(1, exe);
            let neighbour_folder = exe.rsplit_once(['\\', '/']).unwrap().0;
            let sep = if exe.contains('\\') { "\\" } else { "/" };
            let all = vec![
                blocked.clone(),
                process(2, &format!("{neighbour_folder}{sep}other.exe")),
                process(3, &format!("{neighbour_folder}{sep}sub{sep}deep.exe")),
            ];
            assert!(helpers_of(&blocked, &all).is_empty(), "{exe}");
        }
        // Without a known program file, nothing either.
        let unknown = ProcessSnapshot {
            pid: 1,
            name: "x.exe".into(),
            exe: None,
        };
        assert!(helpers_of(&unknown, &[]).is_empty());
    }

    #[test]
    fn a_program_deeper_in_a_user_folder_has_helpers() {
        let discord = process(1, r"C:\Users\me\AppData\Local\Discord\app-1.0\Discord.exe");
        let all = vec![
            discord.clone(),
            process(
                2,
                r"C:\Users\me\AppData\Local\Discord\app-1.0\modules\update.exe",
            ),
            process(3, r"C:\Users\me\AppData\Local\Discord\Update.exe"),
        ];
        let found: Vec<u32> = helpers_of(&discord, &all)
            .iter()
            .map(|helper| helper.pid)
            .collect();
        assert_eq!(found, vec![2]);
    }
}
