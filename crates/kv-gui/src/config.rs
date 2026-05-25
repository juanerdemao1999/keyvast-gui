//! Configuration persistence — save/load GUI settings to TOML.
//!
//! Settings are stored in `<config_dir>/keyvast/gui.toml` where
//! `<config_dir>` is the platform-standard config directory:
//!   - Windows: `%APPDATA%\keyvast\gui.toml`
//!   - macOS:   `~/Library/Application Support/keyvast/gui.toml`
//!   - Linux:   `~/.config/keyvast/gui.toml`
//!
//! On load failure (missing file, parse error) defaults are used silently.
//! On save failure a warning is logged to stderr.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Persistent GUI configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct GuiConfig {
    pub display: DisplayConfig,
    pub filters: FilterConfig,
    pub ui: UiConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DisplayConfig {
    pub visible_channels: usize,
    pub time_window_idx: usize,
    pub amp_scale_idx: usize,
    pub show_grid: bool,
    pub show_channel_labels: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FilterConfig {
    pub hp_enabled: bool,
    pub hp_freq: f64,
    pub lp_enabled: bool,
    pub lp_freq: f64,
    pub notch_enabled: bool,
    pub notch_freq: f64,
    pub car_enabled: bool,
    pub spike_threshold_enabled: bool,
    pub spike_threshold_sigma: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    pub show_spectrum: bool,
    pub show_perf_overlay: bool,
}

// ── Defaults ─────────────────────────────────────────────────────────

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            visible_channels: 16,
            time_window_idx: 2,
            amp_scale_idx: 4,
            show_grid: true,
            show_channel_labels: true,
        }
    }
}

impl Default for FilterConfig {
    fn default() -> Self {
        Self {
            hp_enabled: true,
            hp_freq: 300.0,
            lp_enabled: false,
            lp_freq: 250.0,
            notch_enabled: false,
            notch_freq: 50.0,
            car_enabled: false,
            spike_threshold_enabled: false,
            spike_threshold_sigma: 4.0,
        }
    }
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            show_spectrum: true,
            show_perf_overlay: false,
        }
    }
}

// ── Load / Save ──────────────────────────────────────────────────────

/// Get the path to the config file.
fn config_path() -> Option<PathBuf> {
    let dir = dirs::config_dir()?;
    Some(dir.join("keyvast").join("gui.toml"))
}

/// Load configuration from disk.  Returns defaults on any error.
pub fn load() -> GuiConfig {
    let path = match config_path() {
        Some(p) => p,
        None => return GuiConfig::default(),
    };
    let content = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return GuiConfig::default(),
    };
    toml::from_str(&content).unwrap_or_default()
}

/// Save configuration to disk.  Errors are logged to stderr.
pub fn save(config: &GuiConfig) {
    let path = match config_path() {
        Some(p) => p,
        None => {
            eprintln!("[config] unable to determine config directory");
            return;
        }
    };
    if let Some(parent) = path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        eprintln!("[config] failed to create config dir: {}", e);
        return;
    }
    let content = match toml::to_string_pretty(config) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[config] failed to serialize: {}", e);
            return;
        }
    };
    if let Err(e) = std::fs::write(&path, content) {
        eprintln!("[config] failed to write {}: {}", path.display(), e);
    }
}
