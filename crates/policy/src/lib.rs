use chrono::{DateTime, Utc};
use directories::ProjectDirs;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use thiserror::Error;

/// Processes that must never be terminated (OS stability + self-protection).
pub fn always_protected() -> &'static [&'static str] {
    &[
        "csrss.exe",
        "wininit.exe",
        "winlogon.exe",
        "services.exe",
        "lsass.exe",
        "smss.exe",
        "svchost.exe",
        "system",
        "registry",
        "explorer.exe",
        "dwm.exe",
        "fontdrvhost.exe",
        "sihost.exe",
        "taskhostw.exe",
        "runtimebroker.exe",
        "searchhost.exe",
        "startmenuexperiencehost.exe",
        "shellexperiencehost.exe",
        "applicationframehost.exe",
        "conhost.exe",
        "digital-wellbeing.exe",
        "digital_wellbeing.exe",
    ]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum EnforcementMode {
    #[default]
    Blacklist,
    Whitelist,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Strategy {
    pub name: String,
    pub mode: EnforcementMode,
    pub blacklist: BTreeSet<String>,
    pub whitelist: BTreeSet<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Policy {
    pub mode: EnforcementMode,
    pub blacklist: BTreeSet<String>,
    pub whitelist: BTreeSet<String>,
    #[serde(default)]
    pub strategies: Vec<Strategy>,
    #[serde(default = "Utc::now")]
    pub updated_at: DateTime<Utc>,
}

impl Default for Policy {
    fn default() -> Self {
        let mut whitelist = BTreeSet::new();
        for name in always_protected() {
            whitelist.insert((*name).to_string());
        }
        Self {
            mode: EnforcementMode::Blacklist,
            blacklist: BTreeSet::new(),
            whitelist,
            strategies: Vec::new(),
            updated_at: Utc::now(),
        }
    }
}

impl Policy {
    pub fn normalize_name(name: &str) -> String {
        let trimmed = name.trim().to_lowercase();
        let file_name = Path::new(&trimmed)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(&trimmed);
        if file_name.ends_with(".exe") {
            file_name.to_string()
        } else if file_name.is_empty() {
            trimmed
        } else {
            format!("{file_name}.exe")
        }
    }
    
    pub fn is_protected(name: &str) -> bool {
        let n = Self::normalize_name(name);
        always_protected().iter().any(|p| *p == n)
    }

    pub fn should_block(&self, process_name: &str) -> bool {
        let name = Self::normalize_name(process_name);
        if Self::is_protected(&name) {
            return false;
        }
        match self.mode {
            EnforcementMode::Blacklist => self.blacklist.contains(&name),
            EnforcementMode::Whitelist => !self.whitelist.contains(&name),
        }
    }
}
#[derive(Debug, Error)]
pub enum PolicyError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("cannot modify protected process: {0}")]
    Protected(String),
    #[error("strategy not found: {0}")]
    StrategyNotFound(String),
    #[error("app data directory unavailable")]
    NoAppData,
}

#[derive(Clone)]
pub struct PolicyStore {
    inner: Arc<RwLock<Policy>>,
    path: PathBuf,
}

impl PolicyStore {
    pub fn open_default() -> Result<Self, PolicyError> {
        let dirs = ProjectDirs::from("com", "DigitalWellbeing", "DigitalWellbeing")
            .ok_or(PolicyError::NoAppData)?;
        let dir = dirs.config_dir();
        fs::create_dir_all(dir)?;
        let path = dir.join("policy.json");
        Self::open(path)
    }

    pub fn open(path: PathBuf) -> Result<Self, PolicyError> {
        let policy = if path.exists() {
            let raw = fs::read_to_string(&path)?;
            serde_json::from_str(&raw)?
        } else {
            let policy = Policy::default();
            let raw = serde_json::to_string_pretty(&policy)?;
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&path, raw)?;
            policy
        };
        Ok(Self {
            inner: Arc::new(RwLock::new(policy)),
            path,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn get(&self) -> Policy {
        self.inner.read().clone()
    }

    fn persist_locked(path: &Path, policy: &Policy) -> Result<(), PolicyError> {
        let raw = serde_json::to_string_pretty(policy)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, &raw)?;
        fs::rename(&tmp, path)?;
        Ok(())
    }

    fn mutate<F, T>(&self, f: F) -> Result<T, PolicyError>
    where
        F: FnOnce(&mut Policy) -> Result<T, PolicyError>,
    {
        let mut guard = self.inner.write();
        let result = f(&mut guard)?;
        guard.updated_at = Utc::now();
        Self::persist_locked(&self.path, &guard)?;
        Ok(result)
    }

    pub fn set_mode(&self, mode: EnforcementMode) -> Result<Policy, PolicyError> {
        self.mutate(|p| {
            p.mode = mode;
            Ok(p.clone())
        })
    }

    pub fn add_blacklist(&self, name: &str) -> Result<Policy, PolicyError> {
        let name = Policy::normalize_name(name);
        if Policy::is_protected(&name) {
            return Err(PolicyError::Protected(name));
        }
        self.mutate(|p| {
            p.blacklist.insert(name);
            Ok(p.clone())
        })
    }

    pub fn remove_blacklist(&self, name: &str) -> Result<Policy, PolicyError> {
        let name = Policy::normalize_name(name);
        self.mutate(|p| {
            p.blacklist.remove(&name);
            Ok(p.clone())
        })
    }

    pub fn add_whitelist(&self, name: &str) -> Result<Policy, PolicyError> {
        let name = Policy::normalize_name(name);
        self.mutate(|p| {
            p.whitelist.insert(name);
            Ok(p.clone())
        })
    }

    pub fn remove_whitelist(&self, name: &str) -> Result<Policy, PolicyError> {
        let name = Policy::normalize_name(name);
        if Policy::is_protected(&name) {
            return Err(PolicyError::Protected(name));
        }
        self.mutate(|p| {
            p.whitelist.remove(&name);
            Ok(p.clone())
        })
    }

    pub fn save_strategy(&self, name: &str) -> Result<Policy, PolicyError> {
        let name = name.trim().to_string();
        self.mutate(|p| {
            let strategy = Strategy {
                name: name.clone(),
                mode: p.mode,
                blacklist: p.blacklist.clone(),
                whitelist: p.whitelist.clone(),
            };
            if let Some(existing) = p.strategies.iter_mut().find(|s| s.name == name) {
                *existing = strategy;
            } else {
                p.strategies.push(strategy);
            }
            Ok(p.clone())
        })
    }

    pub fn apply_strategy(&self, name: &str) -> Result<Policy, PolicyError> {
        let name = name.trim().to_string();
        self.mutate(|p| {
            let strategy = p
                .strategies
                .iter()
                .find(|s| s.name == name)
                .cloned()
                .ok_or_else(|| PolicyError::StrategyNotFound(name.clone()))?;
            p.mode = strategy.mode;
            p.blacklist = strategy.blacklist;
            p.whitelist = strategy.whitelist;
            for protected in always_protected() {
                p.whitelist.insert((*protected).to_string());
            }
            Ok(p.clone())
        })
    }

    pub fn replace_policy(&self, mut policy: Policy) -> Result<Policy, PolicyError> {
        for protected in always_protected() {
            policy.whitelist.insert((*protected).to_string());
        }
        policy.updated_at = Utc::now();
        let mut guard = self.inner.write();
        *guard = policy.clone();
        Self::persist_locked(&self.path, &guard)?;
        Ok(policy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_normalize_name() {
        assert_eq!(Policy::normalize_name("folder/steam"), "steam.exe");
        assert_eq!(Policy::normalize_name("Steam"), "steam.exe");
        assert_eq!(Policy::normalize_name("steam.exe"), "steam.exe");
    }

    #[test]
    fn blacklist_blocks_listed() {
        let mut p = Policy::default();
        p.blacklist.insert("steam.exe".into());
        assert!(p.should_block("Steam.EXE"));
        assert!(!p.should_block("notepad.exe"));
        assert!(!p.should_block("explorer.exe"));
    }

    #[test]
    fn whitelist_blocks_unknown() {
        let mut p = Policy::default();
        p.mode = EnforcementMode::Whitelist;
        p.whitelist.insert("notepad.exe".into());
        assert!(!p.should_block("notepad.exe"));
        assert!(p.should_block("steam.exe"));
        assert!(!p.should_block("csrss.exe"));
    }
    #[test]
    fn test_always_protected() {
        let list_of_protected = always_protected();
        let policy = Policy::default();
        for protected in list_of_protected {
            assert!(!policy.should_block(protected));
        }
    }

}
