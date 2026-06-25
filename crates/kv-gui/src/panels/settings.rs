//! Settings state structs for the Keyvast GUI side panels.
//!
//! Extracted from `panels.rs` (Display/Filter/Recording/Device settings).

use std::path::PathBuf;

use eframe::egui;

// ── Display settings state ──────────────────────────────────────────

/// Display mode: how the time axis updates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayMode {
    /// Fixed window, cursor sweeps right (SpikeGLX/Intan RHX default).
    Sweep,
    /// Continuous scrolling, latest data on the right.
    Roll,
}

/// 8-color palette for channel group coloring.
pub const CHANNEL_GROUP_COLORS: &[egui::Color32] = &[
    egui::Color32::from_rgb(100, 180, 255), // blue
    egui::Color32::from_rgb(120, 220, 120), // green
    egui::Color32::from_rgb(255, 160, 80),  // orange
    egui::Color32::from_rgb(200, 120, 255), // purple
    egui::Color32::from_rgb(255, 100, 100), // red
    egui::Color32::from_rgb(80, 220, 200),  // teal
    egui::Color32::from_rgb(255, 220, 80),  // yellow
    egui::Color32::from_rgb(200, 200, 200), // gray
];

/// Time-window presets in seconds (total visible window width).
pub const TIME_WINDOWS: &[f64] = &[1.0, 2.0, 5.0, 10.0, 20.0];

/// Amplitude presets in microvolts per division (display only — raw i16 scaled).
pub const AMP_SCALES: &[f64] = &[50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0];

/// Initial channel toggle capacity.  The vec grows dynamically if the device
/// has more channels, so this is just the default pre-allocation size.
const INITIAL_CHANNEL_TOGGLES: usize = 64;

#[derive(Debug, Clone)]
pub struct DisplaySettings {
    pub visible_channels: usize,
    pub time_scale_idx: usize,
    pub amp_scale_idx: usize,
    pub show_grid: bool,
    pub show_channel_labels: bool,
    #[allow(dead_code)] // planned overlay display mode
    pub overlay_mode: bool,
    /// When true: hovering over a channel highlights it white and dims others.
    /// When false (default): all channels always render at full brightness.
    pub hover_highlight: bool,
    /// Scroll step when browsing history while paused, as a percentage of
    /// the current time window. Default 10 = 10% of window per scroll click.
    pub browse_step_pct: f64,
    /// Per-channel enable/disable (true = visible).
    pub channel_enabled: Vec<bool>,
    /// Vertical spacing between channel baselines (1.0 = dense, 6.0 = spread).
    pub channel_spacing: f64,
    // ── Phase 2 fields ──────────────────────────────────────────────
    /// Display mode: Sweep (default) or Roll.
    pub display_mode: DisplayMode,
    /// Whether to color channels by group (cycling palette).
    pub color_by_group: bool,
    /// Number of channels per color group.
    pub channels_per_group: usize,
    /// Custom channel display order. Empty = natural (identity) order.
    pub channel_order: Vec<usize>,
}

/// Minimum allowed channel spacing.
pub const SPACING_MIN: f64 = 1.0;
/// Maximum allowed channel spacing.
pub const SPACING_MAX: f64 = 6.0;
/// Step for keyboard +/- adjustment.
pub const SPACING_STEP: f64 = 0.2;

impl Default for DisplaySettings {
    fn default() -> Self {
        Self {
            visible_channels: 16,
            time_scale_idx: 2, // 5 s window
            amp_scale_idx: 4,  // 1000 uV/div
            show_grid: true,
            show_channel_labels: true,
            overlay_mode: false,
            hover_highlight: false,
            browse_step_pct: 10.0,
            channel_enabled: vec![true; INITIAL_CHANNEL_TOGGLES],
            channel_spacing: crate::waveform::DEFAULT_CHANNEL_SPACING,
            display_mode: DisplayMode::Sweep,
            color_by_group: false,
            channels_per_group: 8,
            channel_order: Vec::new(),
        }
    }
}

impl DisplaySettings {
    /// Total visible window width in seconds.
    pub fn time_window_secs(&self) -> f64 {
        TIME_WINDOWS[self.time_scale_idx]
    }

    /// Total visible window width in milliseconds.
    pub fn time_window_ms(&self) -> f64 {
        self.time_window_secs() * 1000.0
    }

    pub fn amp_scale_uv(&self) -> f64 {
        AMP_SCALES[self.amp_scale_idx]
    }

    /// Check if a channel is enabled for display.
    pub fn is_channel_enabled(&self, ch: usize) -> bool {
        self.channel_enabled.get(ch).copied().unwrap_or(true)
    }

    /// Get the display color for a channel. If group coloring is enabled,
    /// color cycles through the palette based on group assignment.
    pub fn channel_color(&self, ch: usize) -> egui::Color32 {
        if self.color_by_group && self.channels_per_group > 0 {
            let group = ch / self.channels_per_group;
            CHANNEL_GROUP_COLORS[group % CHANNEL_GROUP_COLORS.len()]
        } else {
            egui::Color32::from_rgb(100, 180, 255) // default blue
        }
    }

    /// Map a display position to a physical channel index.
    /// If `channel_order` is empty, returns the identity mapping.
    #[allow(dead_code)] // inverse of physical_to_display, kept for symmetry
    pub fn display_to_physical(&self, display_pos: usize) -> usize {
        if self.channel_order.is_empty() {
            display_pos
        } else {
            self.channel_order
                .get(display_pos)
                .copied()
                .unwrap_or(display_pos)
        }
    }
}

// ── Filter / signal-processing settings ─────────────────────────────

/// Notch frequency presets — line noise on most regions.
pub const NOTCH_FREQS: &[f64] = &[50.0, 60.0];

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FilterSettings {
    pub hp_enabled: bool,
    pub hp_cutoff_hz: f64,
    pub lp_enabled: bool,
    pub lp_cutoff_hz: f64,
    pub notch_enabled: bool,
    pub notch_idx: usize, // index into NOTCH_FREQS
    pub car_enabled: bool,
    pub spike_threshold_enabled: bool,
    /// Threshold expressed as multiples of channel RMS (negative-going).
    pub spike_threshold_sigma: f64,
}

impl Default for FilterSettings {
    fn default() -> Self {
        Self {
            hp_enabled: false,
            hp_cutoff_hz: 300.0, // standard for spike-band view
            lp_enabled: false,
            lp_cutoff_hz: 250.0, // standard for LFP-band view
            notch_enabled: false,
            notch_idx: 0, // default 50 Hz (CN/EU mains)
            car_enabled: false,
            spike_threshold_enabled: false,
            spike_threshold_sigma: 4.0,
        }
    }
}

impl FilterSettings {
    pub fn notch_freq_hz(&self) -> f64 {
        NOTCH_FREQS[self.notch_idx]
    }

    pub fn any_filter_enabled(&self) -> bool {
        self.hp_enabled || self.lp_enabled || self.notch_enabled
    }
}

// ── Recording state ─────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordingState {
    Idle,
    Armed,
    Recording,
}

#[derive(Debug, Clone)]
pub struct RecordingSettings {
    pub state: RecordingState,
    pub output_dir: String,
    pub file_prefix: String,
    pub recorded_blocks: u64,
    pub recorded_bytes: u64,
}

impl Default for RecordingSettings {
    fn default() -> Self {
        Self {
            state: RecordingState::Idle,
            output_dir: "recordings".to_string(),
            file_prefix: "session".to_string(),
            recorded_blocks: 0,
            recorded_bytes: 0,
        }
    }
}

// ── Device source settings ──────────────────────────────────────────

/// Which acquisition backend Device mode should use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    /// Synthetic simulator — the default; runs without any hardware.
    Simulator,
    /// Real Intan RHD / Opal Kelly XEM7310 board via the `kv-rhd` backend.
    Rhd,
}

/// Upper bound for the RHD headstage (32-channel data stream) count.
pub const RHD_MAX_STREAMS: usize = 2;

/// User-selected acquisition source for Device mode.
#[derive(Debug, Clone)]
pub struct DeviceSettings {
    pub kind: DeviceKind,
    /// FPGA bitfile uploaded when opening the RHD board.
    pub rhd_bitfile: Option<PathBuf>,
    /// Number of 32-channel RHD headstages to enable (1 or 2).
    pub rhd_streams: usize,
}

impl Default for DeviceSettings {
    fn default() -> Self {
        Self {
            kind: DeviceKind::Simulator,
            rhd_bitfile: default_bitfile_path(),
            rhd_streams: RHD_MAX_STREAMS,
        }
    }
}

/// Best-effort default bitfile shipped alongside the workspace, if present.
/// Prefers a KeyVast FPGA build (`keyvast_combined_download.bit`, verified to
/// acquire real data). On the KeyVast PCB the 8 RHD SPI buses are re-routed
/// through the module-IO ring (see `keyvast_top.sv` / `modules.xdc`), so only a
/// KeyVast bitstream reaches the headstage. The stock Intan/Open Ephys build
/// (`intan_rec_controller_7310.bit`) drives SPI on the *original* Intan pins,
/// which are not wired to the headstage on this board — it reads flat `0xFFFF`
/// on every port here, so it is kept only as a last-resort fallback for a
/// genuine Intan recording controller. All three expose the same `board_id=700`
/// data plane this backend speaks. Only a convenience pre-fill — the user can
/// always pick another file, and `None` is returned when none can be located so
/// nothing about acquisition is hard-coded.
fn default_bitfile_path() -> Option<PathBuf> {
    let bitfile_names = kv_rhd::RHD_BITFILE_CANDIDATES;

    // 1. Search relative to the executable (works in deployed builds).
    if let Ok(exe) = std::env::current_exe()
        && let Some(exe_dir) = exe.parent()
    {
        for name in &bitfile_names {
            let candidate = exe_dir.join(name);
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }

    // 2. Search current working directory.
    if let Ok(cwd) = std::env::current_dir() {
        for name in &bitfile_names {
            let candidate = cwd.join(name);
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }

    // 3. Fallback: compile-time source tree (development only).
    #[cfg(debug_assertions)]
    {
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        for name in &bitfile_names {
            if let Ok(path) = manifest.join("../../../..").join(name).canonicalize() {
                return Some(path);
            }
        }
    }

    None
}
