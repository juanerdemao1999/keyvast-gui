//! Config persistence — save/load session settings to a JSON file.
//!
//! Saves filter settings, display preferences, device configuration,
//! channel selection, trigger config, audio monitor settings, and probe
//! map geometry to a config file that persists across sessions.
//!
//! Uses manual JSON serialization (no serde) to maintain consistency with
//! the rest of the codebase.

use std::fs;
use std::path::PathBuf;

use eframe::egui;

use crate::panels::{DisplayMode, FilterSettings, DisplaySettings};
use crate::theme;

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
    // Audio monitor
    pub audio_channel: usize,
    pub audio_volume: f32,
    // Remote API
    pub remote_port: u16,
    // Probe map
    pub probe_geometry: String,
    pub probe_site_radius: f32,
}

impl Default for PersistentConfig {
    fn default() -> Self {
        Self {
            visible_channels: 16,
            time_scale_idx: 2,
            amp_scale_idx: 4,
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
            audio_channel: 0,
            audio_volume: 0.5,
            remote_port: 4444,
            probe_geometry: "linear_dual".to_string(),
            probe_site_radius: 6.0,
        }
    }
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
        && let Some(dir) = exe.parent() {
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
  "audio_channel": {audio_channel},
  "audio_volume": {audio_volume:.2},
  "remote_port": {remote_port},
  "probe_geometry": "{probe_geometry}",
  "probe_site_radius": {probe_site_radius:.1}
}}"#,
            visible_channels = self.visible_channels,
            time_scale_idx = self.time_scale_idx,
            amp_scale_idx = self.amp_scale_idx,
            show_grid = self.show_grid,
            channel_spacing = self.channel_spacing,
            display_mode = self.display_mode,
            color_by_group = self.color_by_group,
            channels_per_group = self.channels_per_group,
            hp_enabled = self.hp_enabled,
            hp_cutoff_hz = self.hp_cutoff_hz,
            lp_enabled = self.lp_enabled,
            lp_cutoff_hz = self.lp_cutoff_hz,
            notch_enabled = self.notch_enabled,
            notch_idx = self.notch_idx,
            car_enabled = self.car_enabled,
            output_dir = self.output_dir.replace('\\', "\\\\").replace('"', "\\\""),
            file_prefix = self.file_prefix.replace('"', "\\\""),
            audio_channel = self.audio_channel,
            audio_volume = self.audio_volume,
            remote_port = self.remote_port,
            probe_geometry = self.probe_geometry,
            probe_site_radius = self.probe_site_radius,
        )
    }

    /// Parse from a JSON string (lenient, missing fields use defaults).
    pub fn from_json(json: &str) -> Self {
        let mut cfg = Self::default();

        if let Some(v) = extract_usize(json, "visible_channels") { cfg.visible_channels = v; }
        if let Some(v) = extract_usize(json, "time_scale_idx") { cfg.time_scale_idx = v; }
        if let Some(v) = extract_usize(json, "amp_scale_idx") { cfg.amp_scale_idx = v; }
        if let Some(v) = extract_bool(json, "show_grid") { cfg.show_grid = v; }
        if let Some(v) = extract_f64(json, "channel_spacing") { cfg.channel_spacing = v; }
        if let Some(v) = extract_string(json, "display_mode") { cfg.display_mode = v; }
        if let Some(v) = extract_bool(json, "color_by_group") { cfg.color_by_group = v; }
        if let Some(v) = extract_usize(json, "channels_per_group") { cfg.channels_per_group = v; }
        if let Some(v) = extract_bool(json, "hp_enabled") { cfg.hp_enabled = v; }
        if let Some(v) = extract_f64(json, "hp_cutoff_hz") { cfg.hp_cutoff_hz = v; }
        if let Some(v) = extract_bool(json, "lp_enabled") { cfg.lp_enabled = v; }
        if let Some(v) = extract_f64(json, "lp_cutoff_hz") { cfg.lp_cutoff_hz = v; }
        if let Some(v) = extract_bool(json, "notch_enabled") { cfg.notch_enabled = v; }
        if let Some(v) = extract_usize(json, "notch_idx") { cfg.notch_idx = v; }
        if let Some(v) = extract_bool(json, "car_enabled") { cfg.car_enabled = v; }
        if let Some(v) = extract_string(json, "output_dir") { cfg.output_dir = v; }
        if let Some(v) = extract_string(json, "file_prefix") { cfg.file_prefix = v; }
        if let Some(v) = extract_usize(json, "audio_channel") { cfg.audio_channel = v; }
        if let Some(v) = extract_f64(json, "audio_volume") { cfg.audio_volume = v as f32; }
        if let Some(v) = extract_usize(json, "remote_port") { cfg.remote_port = v as u16; }
        if let Some(v) = extract_string(json, "probe_geometry") { cfg.probe_geometry = v; }
        if let Some(v) = extract_f64(json, "probe_site_radius") { cfg.probe_site_radius = v as f32; }

        cfg
    }

    /// Capture current settings from the live application state.
    #[allow(clippy::too_many_arguments)]
    pub fn capture_from(
        display: &DisplaySettings,
        filters: &FilterSettings,
        output_dir: &str,
        file_prefix: &str,
        audio_channel: usize,
        audio_volume: f32,
        remote_port: u16,
        probe_geometry: &str,
        probe_site_radius: f32,
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
            audio_channel,
            audio_volume,
            remote_port,
            probe_geometry: probe_geometry.to_string(),
            probe_site_radius,
        }
    }

    /// Apply loaded config to application state.
    #[allow(clippy::too_many_arguments)]
    pub fn apply_to(
        &self,
        display: &mut DisplaySettings,
        filters: &mut FilterSettings,
        output_dir: &mut String,
        file_prefix: &mut String,
        audio_channel: &mut usize,
        audio_volume: &mut f32,
        remote_port: &mut u16,
    ) {
        display.visible_channels = self.visible_channels;
        display.time_scale_idx = self.time_scale_idx.min(crate::panels::TIME_WINDOWS.len() - 1);
        display.amp_scale_idx = self.amp_scale_idx.min(crate::panels::AMP_SCALES.len() - 1);
        display.show_grid = self.show_grid;
        display.channel_spacing = self.channel_spacing;
        display.display_mode = match self.display_mode.as_str() {
            "roll" => DisplayMode::Roll,
            _ => DisplayMode::Sweep,
        };
        display.color_by_group = self.color_by_group;
        display.channels_per_group = self.channels_per_group;

        filters.hp_enabled = self.hp_enabled;
        filters.hp_cutoff_hz = self.hp_cutoff_hz;
        filters.lp_enabled = self.lp_enabled;
        filters.lp_cutoff_hz = self.lp_cutoff_hz;
        filters.notch_enabled = self.notch_enabled;
        filters.notch_idx = self.notch_idx;
        filters.car_enabled = self.car_enabled;

        *output_dir = self.output_dir.clone();
        *file_prefix = self.file_prefix.clone();
        *audio_channel = self.audio_channel;
        *audio_volume = self.audio_volume;
        *remote_port = self.remote_port;
    }
}

// ── File I/O ────────────────────────────────────────────────────────

/// Save config to disk.
pub fn save_config(path: &PathBuf, config: &PersistentConfig) -> Result<(), String> {
    let json = config.to_json();
    fs::write(path, json).map_err(|e| format!("Failed to save config: {e}"))
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
    save_clicked: &mut bool,
    load_clicked: &mut bool,
) {
    egui::CollapsingHeader::new(
        egui::RichText::new("CONFIG")
            .size(11.0)
            .strong()
            .color(theme::TEXT_SECONDARY),
    )
    .default_open(false)
    .show(ui, |ui| {
        // Auto-save toggle
        ui.checkbox(
            &mut state.auto_save,
            egui::RichText::new("Auto-save on change").size(10.0),
        );

        // Manual save/load buttons
        ui.horizontal(|ui| {
            if ui.button(egui::RichText::new("Save").size(10.0)).clicked() {
                *save_clicked = true;
            }
            if ui.button(egui::RichText::new("Load").size(10.0)).clicked() {
                *load_clicked = true;
            }
        });

        // Config path display
        ui.label(
            egui::RichText::new(format!("File: {}", state.config_path.display()))
                .size(9.0)
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
                    .size(9.0)
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
    let end = after.find(|c: char| !c.is_ascii_digit() && c != '.' && c != '-')
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
        assert_eq!(cfg.time_scale_idx, 2); // default
        assert!(!cfg.hp_enabled); // default
    }

    #[test]
    fn extract_helpers() {
        let json = r#"{"name": "test", "count": 42, "enabled": true, "rate": 3.14}"#;
        assert_eq!(extract_string(json, "name"), Some("test".to_string()));
        assert_eq!(extract_usize(json, "count"), Some(42));
        assert_eq!(extract_bool(json, "enabled"), Some(true));
        assert!((extract_f64(json, "rate").unwrap() - 3.14).abs() < 0.001);
    }

    #[test]
    fn default_config_path_is_reasonable() {
        let p = default_config_path();
        assert!(p.to_string_lossy().contains("keyvast_config.json"));
    }
}
