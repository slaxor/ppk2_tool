//! Daemon settings, persisted as TOML under the XDG config dir.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::Error;
use crate::cmd::PowerMode;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub mode: PowerMode,
    pub millivolts: u16,
    pub fold: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            mode: PowerMode::Source,
            millivolts: 3300,
            fold: 10,
        }
    }
}

impl Settings {
    pub fn path() -> PathBuf {
        config_dir().join("ppk2_tool").join("settings.toml")
    }

    /// Stored settings, or defaults when there is no file yet.
    pub fn load() -> Result<Self, Error> {
        match std::fs::read_to_string(Self::path()) {
            Ok(text) => toml::from_str(&text).map_err(|e| Error::BadSettings(e.to_string())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save(&self) -> Result<(), Error> {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = toml::to_string_pretty(self).map_err(|e| Error::BadSettings(e.to_string()))?;
        std::fs::write(path, text)?;
        Ok(())
    }
}

fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Where the daemon listens: `$XDG_RUNTIME_DIR/ppk2_tool.sock`, or a per-user path under /tmp.
pub fn socket_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join(format!("ppk2_tool-{}", whoami())));
    dir.join("ppk2_tool.sock")
}

fn whoami() -> String {
    std::env::var("USER").unwrap_or_else(|_| "user".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_through_toml() {
        let s = Settings {
            mode: PowerMode::Ampere,
            millivolts: 1800,
            fold: 100,
        };
        let text = toml::to_string(&s).expect("serialises");
        assert_eq!(toml::from_str::<Settings>(&text).expect("parses"), s);
    }

    #[test]
    fn missing_keys_fall_back_to_defaults() {
        let s: Settings = toml::from_str("fold = 7\n").expect("partial file parses");
        assert_eq!(s.fold, 7);
        assert_eq!(s.millivolts, 3300);
        assert_eq!(s.mode, PowerMode::Source);
    }
}
