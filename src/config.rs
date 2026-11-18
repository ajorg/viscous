//! Persisted preset labels, saved as TOML in the user's config directory.

use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{
    focus::{self, MAX_FOCUS_SPEED},
    pan_tilt::{self, DEFAULT_TOP_SPEED, PadLayout, TopSpeeds},
    zoom::{self, MAX_ZOOM_SPEED},
};

/// What a front end remembers between runs: the operator's own words for
/// each preset and title, and which camera to reach for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    /// The camera last connected to — a serial port name or a `tcp://`
    /// endpoint, in the same spelling either front end takes. Absent until
    /// something has actually connected.
    #[serde(default)]
    pub camera: Option<String>,
    /// Whether the window draws a fine line across the pad and the rockers
    /// wherever a speed begins. Off unless asked for, and asked for here
    /// rather than in the window: `speed_lines = true`.
    ///
    /// Written out even when off, so that the file itself says the setting
    /// exists to anyone who opens it to look.
    #[serde(default)]
    pub speed_lines: bool,
    /// How the pad and the stick turn a push into pan and tilt:
    /// `pad_layout = "rings"`, the default, or `"equal-areas"`, the
    /// experiment of every region being exactly the same size.
    #[serde(default)]
    pub pad_layout: PadLayout,
    /// The fastest the pad, the stick and the keys drive each axis, as VISCA
    /// speed numbers: up to 24 for pan and 20 for tilt, though past about 9
    /// the steps stop being ones an eye can tell apart (see
    /// [`DEFAULT_TOP_SPEED`]). Read at start-up.
    ///
    /// Only the equal-areas layout can give the two axes different numbers of
    /// steps; the rings layout drives both from the lower of the two.
    #[serde(default = "default_top_speed")]
    pub pan_top_speed: u8,
    #[serde(default = "default_top_speed")]
    pub tilt_top_speed: u8,
    /// The fastest the zoom and focus rockers (and their keys and triggers)
    /// drive, as VISCA speed numbers from 0, the slowest, to 7. Read at
    /// start-up.
    #[serde(default = "default_lens_top_speed")]
    pub zoom_top_speed: u8,
    #[serde(default = "default_lens_top_speed")]
    pub focus_top_speed: u8,
    /// Preset number -> label.
    #[serde(default)]
    pub presets: BTreeMap<u8, String>,
    /// Title slot number -> the text to burn into the video output.
    #[serde(default)]
    pub titles: BTreeMap<u8, String>,
}

fn default_top_speed() -> u8 {
    DEFAULT_TOP_SPEED
}

fn default_lens_top_speed() -> u8 {
    MAX_ZOOM_SPEED
}

impl Default for Config {
    fn default() -> Self {
        Self {
            camera: None,
            speed_lines: false,
            pad_layout: PadLayout::default(),
            pan_top_speed: DEFAULT_TOP_SPEED,
            tilt_top_speed: DEFAULT_TOP_SPEED,
            zoom_top_speed: MAX_ZOOM_SPEED,
            focus_top_speed: MAX_FOCUS_SPEED,
            presets: BTreeMap::new(),
            titles: BTreeMap::new(),
        }
    }
}

impl Config {
    /// The pan and tilt top speeds this config asks for.
    pub fn top_speeds(&self) -> TopSpeeds {
        TopSpeeds {
            pan: self.pan_top_speed,
            tilt: self.tilt_top_speed,
        }
    }

    /// Puts every top speed this config asks for into use for the rest of the
    /// run. Has to come before anything drives, or draws a control with its
    /// speeds marked: each is fixed the first time it is read.
    pub fn use_top_speeds(&self) {
        pan_tilt::use_top_speeds(self.top_speeds());
        zoom::TOP_SPEED.set(self.zoom_top_speed);
        focus::TOP_SPEED.set(self.focus_top_speed);
    }
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
    }

    #[test]
    fn speed_lines_are_off_until_the_file_asks_for_them() {
        let path = test_path("speed-lines");
        fs::write(&path, "[presets]\n1 = \"wide shot\"\n").expect("write should succeed");
        let unasked = load(&path).expect("a config without the setting should load");
        assert!(!unasked.speed_lines);

        fs::write(
            &path,
            "speed_lines = true\n\n[presets]\n1 = \"wide shot\"\n",
        )
        .expect("write should succeed");
        let asked = load(&path).expect("a config with the setting should load");
        assert!(asked.speed_lines);

        // And saving keeps the setting, with the labels still under it.
        save(&asked, &path).expect("save should succeed");
        let kept = load(&path).expect("what was saved should load");
        let _ = fs::remove_file(&path);
        assert_eq!(kept, asked);
    }

    #[test]
    fn the_pad_layout_is_rings_unless_the_file_says_otherwise() {
        let path = test_path("pad-layout");
        fs::write(&path, "[presets]\n1 = \"wide shot\"\n").expect("write should succeed");
        assert_eq!(load(&path).unwrap().pad_layout, PadLayout::Rings);

        fs::write(&path, "pad_layout = \"equal-areas\"\n").expect("write should succeed");
        let asked = load(&path).expect("a config naming a layout should load");
        assert_eq!(asked.pad_layout, PadLayout::EqualAreas);

        save(&asked, &path).expect("save should succeed");
        let kept = load(&path).expect("what was saved should load");
        let _ = fs::remove_file(&path);
        assert_eq!(kept, asked);
    }

    #[test]
    fn top_speeds_are_nine_unless_the_file_says_otherwise() {
        let path = test_path("top-speeds");
        fs::write(&path, "[presets]\n1 = \"wide shot\"\n").expect("write should succeed");
        let unasked = load(&path).expect("a config without the setting should load");
        assert_eq!(unasked.top_speeds(), TopSpeeds { pan: 9, tilt: 9 });

        assert_eq!((unasked.zoom_top_speed, unasked.focus_top_speed), (7, 7));

        fs::write(
            &path,
            "pan_top_speed = 11\ntilt_top_speed = 7\nzoom_top_speed = 6\nfocus_top_speed = 5\n",
        )
        .expect("write should succeed");
        let asked = load(&path).expect("a config with the setting should load");
        assert_eq!(asked.top_speeds(), TopSpeeds { pan: 11, tilt: 7 });
        assert_eq!((asked.zoom_top_speed, asked.focus_top_speed), (6, 5));

        save(&asked, &path).expect("save should succeed");
        let kept = load(&path).expect("what was saved should load");
        let _ = fs::remove_file(&path);
        assert_eq!(kept, asked);
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
