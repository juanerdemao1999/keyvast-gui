//! Config persistence — save/load session settings to a JSON file.
//!
//! Saves filter settings, display preferences, device configuration,
//! channel selection, and trigger config to a config file that persists
//! across sessions.
//!
//! Uses manual JSON serialization (no serde) to maintain consistency with
//! the rest of the codebase.

use std::fs;
use std::path::PathBuf;

use eframe::egui;

use crate::panels::{DisplayMode, DisplaySettings, FilterSettings};
use crate::theme;

// ── UI scale bounds ─────────────────────────────────────────────────

/// Minimum UI scale factor (`set_pixels_per_point`).
pub const UI_SCALE_MIN: f32 = 0.8;
/// Maximum UI scale factor (slider upper bound only).
pub const UI_SCALE_MAX: f32 = 1.6;
/// Neutral first-launch / Reset scale. Previously the console launched at
/// `UI_SCALE_MAX`, so it opened oversized and could only ever be shrunk, and
/// Reset jumped back to max rather than to a sensible middle (C33).
pub const UI_SCALE_DEFAULT: f32 = 1.0;

// ── Config data model ───────────────────────────────────────────────

/// Subset of application settings that are persisted across sessions.
#[derive(Debug, Clone)]
pub struct PersistentConfig {
    // Display
    pub visible_channels: usize,
    pub time_scale_idx: usize,
    pub amp_scale_idx: usize,
    pub show_grid: bool,
    pub channel_spacing: f64,
    pub display_mode: String, // "sweep" or "roll"
    pub color_by_group: bool,
    pub channels_per_group: usize,
    // Filters
    pub hp_enabled: bool,
    pub hp_cutoff_hz: f64,
    pub lp_enabled: bool,
    pub lp_cutoff_hz: f64,
    pub notch_enabled: bool,
    pub notch_idx: usize,
    pub car_enabled: bool,
    // Recording
    pub output_dir: String,
    pub file_prefix: String,
    // Remote API
    pub remote_port: u16,
    // App / window (#15, #17)
    pub ui_scale: f32,
    pub window_width: f32,
    pub window_height: f32,
    pub last_source: String, // "demo", "device" or "playback"
}

impl Default for PersistentConfig {
    fn default() -> Self {
        Self {
            visible_channels: 16,
            time_scale_idx: crate::panels::DEFAULT_TIME_WINDOW_IDX,
            amp_scale_idx: 3,
            show_grid: true,
            channel_spacing: 3.0,
            display_mode: "sweep".to_string(),
            color_by_group: false,
            channels_per_group: 8,
            hp_enabled: false,
            hp_cutoff_hz: 300.0,
            lp_enabled: false,
            lp_cutoff_hz: 250.0,
            notch_enabled: false,
            notch_idx: 0,
            car_enabled: false,
            output_dir: "recordings".to_string(),
            file_prefix: "session".to_string(),
            remote_port: 4444,
            ui_scale: UI_SCALE_DEFAULT,
            window_width: 1200.0,
            window_height: 800.0,
            last_source: "demo".to_string(),
        }
    }
}

/// Load the persisted config from its default location, falling back to
/// defaults if no file exists yet.  Used at startup before the app is built.
pub fn load_or_default() -> PersistentConfig {
    load_config(&default_config_path()).unwrap_or_default()
}

// ── Persistence state ───────────────────────────────────────────────

pub struct ConfigPersistState {
    /// Path to the config file.
    pub config_path: PathBuf,
    /// Whether auto-save is enabled (save on settings change).
    pub auto_save: bool,
    /// Last save/load status message.
    pub status_message: Option<String>,
    /// Whether config was successfully loaded at startup.
    pub loaded: bool,
}

impl Default for ConfigPersistState {
    fn default() -> Self {
        Self {
            config_path: default_config_path(),
            auto_save: true,
            status_message: None,
            loaded: false,
        }
    }
}

/// Default config file location (next to executable or in CWD).
fn default_config_path() -> PathBuf {
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        return dir.join("keyvast_config.json");
    }
    PathBuf::from("keyvast_config.json")
}

// ── Serialization (manual JSON) ─────────────────────────────────────

impl PersistentConfig {
    /// Serialize to a JSON string.
    pub fn to_json(&self) -> String {
        format!(
            r#"{{
  "visible_channels": {visible_channels},
  "time_scale_idx": {time_scale_idx},
  "amp_scale_idx": {amp_scale_idx},
  "show_grid": {show_grid},
  "channel_spacing": {channel_spacing:.2},
  "display_mode": "{display_mode}",
  "color_by_group": {color_by_group},
  "channels_per_group": {channels_per_group},
  "hp_enabled": {hp_enabled},
  "hp_cutoff_hz": {hp_cutoff_hz:.1},
  "lp_enabled": {lp_enabled},
  "lp_cutoff_hz": {lp_cutoff_hz:.1},
  "notch_enabled": {notch_enabled},
  "notch_idx": {notch_idx},
  "car_enabled": {car_enabled},
  "output_dir": "{output_dir}",
  "file_prefix": "{file_prefix}",
  "remote_port": {remote_port},
  "ui_scale": {ui_scale:.2},
  "window_width": {window_width:.0},
  "window_height": {window_height:.0},
  "last_source": "{last_source}"
}}"#,
            visible_channels = self.visible_channels,
            time_scale_idx = self.time_scale_idx,
            amp_scale_idx = self.amp_scale_idx,
            show_grid = self.show_grid,
            channel_spacing = self.channel_spacing,
            color_by_group = self.color_by_group,
            channels_per_group = self.channels_per_group,
            hp_enabled = self.hp_enabled,
            hp_cutoff_hz = self.hp_cutoff_hz,
            lp_enabled = self.lp_enabled,
            lp_cutoff_hz = self.lp_cutoff_hz,
            notch_enabled = self.notch_enabled,
            notch_idx = self.notch_idx,
            car_enabled = self.car_enabled,
            output_dir = json_escape(&self.output_dir),
            file_prefix = json_escape(&self.file_prefix),
            remote_port = self.remote_port,
            ui_scale = self.ui_scale,
            window_width = self.window_width,
            window_height = self.window_height,
            display_mode = json_escape(&self.display_mode),
            last_source = json_escape(&self.last_source),
        )
    }

    /// Parse from a JSON string (lenient, missing fields use defaults).
    pub fn from_json(json: &str) -> Self {
        let mut cfg = Self::default();

        if let Some(v) = extract_usize(json, "visible_channels") {
            cfg.visible_channels = v;
        }
        if let Some(v) = extract_usize(json, "time_scale_idx") {
            cfg.time_scale_idx = v;
        }
        if let Some(v) = extract_usize(json, "amp_scale_idx") {
            cfg.amp_scale_idx = v;
        }
        if let Some(v) = extract_bool(json, "show_grid") {
            cfg.show_grid = v;
        }
        if let Some(v) = extract_f64(json, "channel_spacing") {
            cfg.channel_spacing = v;
        }
        if let Some(v) = extract_string(json, "display_mode") {
            cfg.display_mode = v;
        }
        if let Some(v) = extract_bool(json, "color_by_group") {
            cfg.color_by_group = v;
        }
        if let Some(v) = extract_usize(json, "channels_per_group") {
            cfg.channels_per_group = v;
        }
        if let Some(v) = extract_bool(json, "hp_enabled") {
            cfg.hp_enabled = v;
        }
        if let Some(v) = extract_f64(json, "hp_cutoff_hz") {
            cfg.hp_cutoff_hz = v;
        }
        if let Some(v) = extract_bool(json, "lp_enabled") {
            cfg.lp_enabled = v;
        }
        if let Some(v) = extract_f64(json, "lp_cutoff_hz") {
            cfg.lp_cutoff_hz = v;
        }
        if let Some(v) = extract_bool(json, "notch_enabled") {
            cfg.notch_enabled = v;
        }
        if let Some(v) = extract_usize(json, "notch_idx") {
            cfg.notch_idx = v;
        }
        if let Some(v) = extract_bool(json, "car_enabled") {
            cfg.car_enabled = v;
        }
        if let Some(v) = extract_string(json, "output_dir") {
            cfg.output_dir = v;
        }
        if let Some(v) = extract_string(json, "file_prefix") {
            cfg.file_prefix = v;
        }
        if let Some(v) = extract_usize(json, "remote_port") {
            cfg.remote_port = v as u16;
        }
        if let Some(v) = extract_f64(json, "ui_scale") {
            cfg.ui_scale = v as f32;
        }
        if let Some(v) = extract_f64(json, "window_width") {
            cfg.window_width = v as f32;
        }
        if let Some(v) = extract_f64(json, "window_height") {
            cfg.window_height = v as f32;
        }
        if let Some(v) = extract_string(json, "last_source") {
            cfg.last_source = v;
        }

        cfg
    }

    /// Capture current settings from the live application state.
    pub fn capture_from(
        display: &DisplaySettings,
        filters: &FilterSettings,
        output_dir: &str,
        file_prefix: &str,
        remote_port: u16,
    ) -> Self {
        Self {
            visible_channels: display.visible_channels,
            time_scale_idx: display.time_scale_idx,
            amp_scale_idx: display.amp_scale_idx,
            show_grid: display.show_grid,
            channel_spacing: display.channel_spacing,
            display_mode: match display.display_mode {
                DisplayMode::Sweep => "sweep".to_string(),
                DisplayMode::Roll => "roll".to_string(),
            },
            color_by_group: display.color_by_group,
            channels_per_group: display.channels_per_group,
            hp_enabled: filters.hp_enabled,
            hp_cutoff_hz: filters.hp_cutoff_hz,
            lp_enabled: filters.lp_enabled,
            lp_cutoff_hz: filters.lp_cutoff_hz,
            notch_enabled: filters.notch_enabled,
            notch_idx: filters.notch_idx,
            car_enabled: filters.car_enabled,
            output_dir: output_dir.to_string(),
            file_prefix: file_prefix.to_string(),
            remote_port,
            // #15/#17 fields are filled in by the caller (capture_persistent);
            // start from the defaults so this constructor stays valid.
            ..Self::default()
        }
    }

    /// Apply loaded config to application state.
    pub fn apply_to(
        &self,
        display: &mut DisplaySettings,
        filters: &mut FilterSettings,
        output_dir: &mut String,
        file_prefix: &mut String,
        remote_port: &mut u16,
    ) {
        // Clamp to ≥1: a persisted/hand-edited 0 would blank the waveform until
        // the user drags the Channels slider (L14).
        display.visible_channels = self.visible_channels.max(1);
        display.time_scale_idx = self
            .time_scale_idx
            .min(crate::panels::TIME_WINDOWS.len() - 1);
        display.amp_scale_idx = self.amp_scale_idx.min(crate::panels::AMP_SCALES.len() - 1);
        display.show_grid = self.show_grid;
        display.channel_spacing = self
            .channel_spacing
            .clamp(crate::panels::SPACING_MIN, crate::panels::SPACING_MAX);
        display.display_mode = match self.display_mode.as_str() {
            "roll" => DisplayMode::Roll,
            _ => DisplayMode::Sweep,
        };
        display.color_by_group = self.color_by_group;
        // Clamp to ≥1: a persisted 0 would silently disable group coloring (I4).
        display.channels_per_group = self.channels_per_group.max(1);

        filters.hp_enabled = self.hp_enabled;
        filters.hp_cutoff_hz = self.hp_cutoff_hz;
        filters.lp_enabled = self.lp_enabled;
        filters.lp_cutoff_hz = self.lp_cutoff_hz;
        filters.notch_enabled = self.notch_enabled;
        filters.notch_idx = self
            .notch_idx
            .min(crate::panels::NOTCH_FREQS.len().saturating_sub(1));
        filters.car_enabled = self.car_enabled;

        *output_dir = self.output_dir.clone();
        *file_prefix = self.file_prefix.clone();
        *remote_port = self.remote_port;
    }
}

// ── File I/O ────────────────────────────────────────────────────────

/// Escape a string for safe embedding inside a double-quoted JSON string.
/// Backslash first so the escapes we add afterwards are not double-escaped.
fn json_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Save config to disk atomically: write to a sibling temp file, then rename
/// over the target so a crash mid-write cannot leave a truncated config.
pub fn save_config(path: &PathBuf, config: &PersistentConfig) -> Result<(), String> {
    let json = config.to_json();
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, json).map_err(|e| format!("Failed to save config: {e}"))?;
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("Failed to save config: {e}")
    })
}

/// Load config from disk.
pub fn load_config(path: &PathBuf) -> Result<PersistentConfig, String> {
    let json = fs::read_to_string(path).map_err(|e| format!("Failed to read config: {e}"))?;
    Ok(PersistentConfig::from_json(&json))
}

// ── GUI drawing ─────────────────────────────────────────────────────

/// Draw the config persistence section in the sidebar.
pub fn draw_config_section(
    ui: &mut egui::Ui,
    state: &mut ConfigPersistState,
    ui_scale: &mut f32,
    save_clicked: &mut bool,
    load_clicked: &mut bool,
) {
    egui::CollapsingHeader::new(
        egui::RichText::new("CONFIG")
            .size(theme::FONT_HEADING)
            .strong()
            .color(theme::TEXT_SECONDARY),
    )
    .default_open(false)
    .show(ui, |ui| {
        // UI scale (#17) — scales the whole interface for high-DPI or distance
        // viewing.  Applied live and persisted with the rest of the config.
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("UI scale").size(theme::FONT_BODY));
            ui.add(
                egui::Slider::new(ui_scale, UI_SCALE_MIN..=UI_SCALE_MAX)
                    .step_by(0.05)
                    .fixed_decimals(2),
            );
            if ui
                .button(egui::RichText::new("Reset").size(theme::FONT_BODY))
                .on_hover_text("Reset UI scale to 1.0")
                .clicked()
            {
                *ui_scale = UI_SCALE_DEFAULT;
            }
        });

        // Auto-save toggle
        ui.checkbox(
            &mut state.auto_save,
            egui::RichText::new("Auto-save on change").size(theme::FONT_BODY),
        );

        // Manual save/load buttons. Load is destructive (it replaces every live
        // setting), so it is spelled out and carries an explicit warning tooltip
        // to reduce mis-clicks next to Save (C22).
        ui.horizontal(|ui| {
            if ui
                .button(egui::RichText::new("Save").size(theme::FONT_BODY))
                .on_hover_text("Write current settings to the config file")
                .clicked()
            {
                *save_clicked = true;
            }
            if ui
                .button(egui::RichText::new("Load\u{2026}").size(theme::FONT_BODY))
                .on_hover_text("Replace ALL current live settings with the saved config file")
                .clicked()
            {
                *load_clicked = true;
            }
        });

        // Config path display
        ui.label(
            egui::RichText::new(format!("File: {}", state.config_path.display()))
                .size(theme::FONT_CAPTION)
                .color(theme::TEXT_DIM),
        );

        // Status message
        if let Some(ref msg) = state.status_message {
            let color = if msg.starts_with("Error") || msg.starts_with("Failed") {
                theme::ACCENT_RED
            } else {
                theme::ACCENT_GREEN
            };
            ui.label(
                egui::RichText::new(msg.as_str())
                    .size(theme::FONT_CAPTION)
                    .color(color),
            );
        }
    });
}

// ── JSON extraction helpers (minimal, no serde) ─────────────────────

fn extract_string(json: &str, key: &str) -> Option<String> {
    let pattern = format!("\"{}\"", key);
    let idx = json.find(&pattern)?;
    let after = &json[idx + pattern.len()..];
    // Skip : and whitespace, find opening quote
    let after = after.trim_start().strip_prefix(':')?;
    let after = after.trim_start();
    if !after.starts_with('"') {
        return None;
    }
    let after = &after[1..]; // skip opening quote
    let end = after.find('"')?;
    Some(after[..end].replace("\\\\", "\\").replace("\\\"", "\""))
}

fn extract_f64(json: &str, key: &str) -> Option<f64> {
    let pattern = format!("\"{}\"", key);
    let idx = json.find(&pattern)?;
    let after = &json[idx + pattern.len()..];
    let after = after.trim_start().strip_prefix(':')?;
    let after = after.trim_start();
    // Parse until non-numeric
    let end = after
        .find(|c: char| !c.is_ascii_digit() && c != '.' && c != '-')
        .unwrap_or(after.len());
    after[..end].parse().ok()
}

fn extract_usize(json: &str, key: &str) -> Option<usize> {
    extract_f64(json, key).map(|v| v as usize)
}

fn extract_bool(json: &str, key: &str) -> Option<bool> {
    let pattern = format!("\"{}\"", key);
    let idx = json.find(&pattern)?;
    let after = &json[idx + pattern.len()..];
    let after = after.trim_start().strip_prefix(':')?;
    let after = after.trim_start();
    if after.starts_with("true") {
        Some(true)
    } else if after.starts_with("false") {
        Some(false)
    } else {
        None
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_json() {
        let cfg = PersistentConfig {
            visible_channels: 32,
            hp_enabled: true,
            hp_cutoff_hz: 500.0,
            output_dir: "C:\\data\\recordings".to_string(),
            ..Default::default()
        };
        let json = cfg.to_json();
        let loaded = PersistentConfig::from_json(&json);
        assert_eq!(loaded.visible_channels, 32);
        assert!(loaded.hp_enabled);
        assert!((loaded.hp_cutoff_hz - 500.0).abs() < 0.01);
        assert_eq!(loaded.output_dir, "C:\\data\\recordings");
    }

    #[test]
    fn missing_fields_use_defaults() {
        let json = r#"{"visible_channels": 8}"#;
        let cfg = PersistentConfig::from_json(json);
        assert_eq!(cfg.visible_channels, 8);
        assert_eq!(cfg.time_scale_idx, crate::panels::DEFAULT_TIME_WINDOW_IDX); // default 5 s
        assert!(!cfg.hp_enabled); // default
    }

    #[test]
    fn extract_helpers() {
        let json = r#"{"name": "test", "count": 42, "enabled": true, "rate": 2.5}"#;
        assert_eq!(extract_string(json, "name"), Some("test".to_string()));
        assert_eq!(extract_usize(json, "count"), Some(42));
        assert_eq!(extract_bool(json, "enabled"), Some(true));
        assert!((extract_f64(json, "rate").unwrap() - 2.5).abs() < 0.001);
    }

    #[test]
    fn default_config_path_is_reasonable() {
        let p = default_config_path();
        assert!(p.to_string_lossy().contains("keyvast_config.json"));
    }
}
