//! Persisted preset labels, saved as TOML in the user's config directory.

use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

/// Preset labels, keyed by the same 1-based preset number used elsewhere.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Config {
    /// Preset number -> label.
    #[serde(default)]
    pub presets: BTreeMap<u8, String>,
}

/// An error loading or saving the config file.
#[derive(Debug)]
pub enum ConfigError {
    /// The file exists but couldn't be read.
    Read(io::Error),
    /// The file couldn't be written.
    Write(io::Error),
    /// The file's contents aren't valid TOML for this shape.
    Parse(toml::de::Error),
    /// The config couldn't be serialized to TOML.
    Serialize(toml::ser::Error),
    /// No config directory is available on this platform.
    NoConfigDir,
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read(e) => write!(f, "failed to read config file: {e}"),
            Self::Write(e) => write!(f, "failed to write config file: {e}"),
            Self::Parse(e) => write!(f, "failed to parse config file: {e}"),
            Self::Serialize(e) => write!(f, "failed to serialize config: {e}"),
            Self::NoConfigDir => {
                write!(f, "couldn't determine a config directory for this platform")
            }
        }
    }
}

impl std::error::Error for ConfigError {}

/// The default path for the config file, following platform convention
/// (e.g. `~/.config/viscous/config.toml` on Linux).
pub fn default_path() -> Result<PathBuf, ConfigError> {
    let dirs = directories::ProjectDirs::from("", "", "viscous").ok_or(ConfigError::NoConfigDir)?;
    Ok(dirs.config_dir().join("config.toml"))
}

/// Loads the config from `path`, or returns an empty default if the file
/// doesn't exist yet (e.g. on first run).
pub fn load(path: &Path) -> Result<Config, ConfigError> {
    match fs::read_to_string(path) {
        Ok(contents) => toml::from_str(&contents).map_err(ConfigError::Parse),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(ConfigError::Read(e)),
    }
}

/// Saves the config to `path`, creating its parent directory if needed.
pub fn save(config: &Config, path: &Path) -> Result<(), ConfigError> {
    let contents = toml::to_string_pretty(config).map_err(ConfigError::Serialize)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(ConfigError::Write)?;
    }
    fs::write(path, contents).map_err(ConfigError::Write)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("viscous-test-config-{name}.toml"))
    }

    #[test]
    fn load_missing_file_returns_default_config() {
        let path = test_path("missing");
        let _ = fs::remove_file(&path);

        let config = load(&path).expect("a missing file should not be an error");

        assert_eq!(config, Config::default());
    }

    #[test]
    fn save_then_load_round_trips_labels() {
        let path = test_path("round-trip");
        let mut config = Config::default();
        config.presets.insert(1, "wide shot".to_string());
        config.presets.insert(3, "podium".to_string());

        save(&config, &path).expect("save should succeed");
        let loaded = load(&path).expect("load should succeed");
        let _ = fs::remove_file(&path);

        assert_eq!(loaded, config);
    }

    #[test]
    fn parsing_rejects_malformed_toml() {
        let path = test_path("malformed");
        fs::write(&path, "not valid toml [[[").expect("write should succeed");

        let result = load(&path);
        let _ = fs::remove_file(&path);

        assert!(matches!(result, Err(ConfigError::Parse(_))));
    }
}
