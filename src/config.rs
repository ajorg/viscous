//! Persisted preset labels, saved as TOML in the user's config directory.

use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::state::Position;

/// What a front end remembers between runs: the operator's own words for
/// each preset and title, and which camera to reach for.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Config {
    /// The camera last connected to — a serial port name or a `tcp://`
    /// endpoint, in the same spelling either front end takes. Absent until
    /// something has actually connected.
    #[serde(default)]
    pub camera: Option<String>,
    /// Preset number -> label.
    #[serde(default)]
    pub presets: BTreeMap<u8, String>,
    /// Preset number -> where that preset was last seen to point.
    ///
    /// Kept apart from [`Self::presets`] rather than folded into it because
    /// the two come from opposite directions and neither implies the other: a
    /// label is typed by the operator and can name a preset the camera has
    /// never been sent to, while a position is read back from the camera and
    /// exists for presets nobody has bothered to name.
    #[serde(default)]
    pub preset_positions: BTreeMap<u8, Position>,
    /// Title slot number -> the text to burn into the video output.
    #[serde(default)]
    pub titles: BTreeMap<u8, String>,
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
    fn save_then_load_round_trips_what_a_preset_is_and_where_it_points() {
        let path = test_path("round-trip");
        let mut config = Config::default();
        config.presets.insert(1, "wide shot".to_string());
        config.presets.insert(3, "podium".to_string());
        // A harvested position for a preset that was never labelled, and a
        // label for one that has never been travelled to: the two halves are
        // kept for their own sake and neither waits on the other.
        config.preset_positions.insert(
            1,
            Position {
                pan: -120,
                tilt: 45,
                zoom: 0x1000,
                focus: 0x2000,
            },
        );

        save(&config, &path).expect("save should succeed");
        let loaded = load(&path).expect("load should succeed");
        let _ = fs::remove_file(&path);

        assert_eq!(loaded, config);
    }

    #[test]
    fn a_config_written_before_cameras_were_remembered_still_loads() {
        let path = test_path("no-camera");
        fs::write(&path, "[presets]\n1 = \"wide shot\"\n").expect("write should succeed");

        let config = load(&path).expect("an older config should still load");
        let _ = fs::remove_file(&path);

        assert_eq!(config.camera, None);
        assert_eq!(
            config.presets.get(&1).map(String::as_str),
            Some("wide shot")
        );
        assert!(config.preset_positions.is_empty());
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
