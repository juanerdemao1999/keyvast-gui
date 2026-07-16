//! Spike waveform overlay — threshold-triggered snippet accumulator + renderer.
//!
//! ## Data flow
//!
//! ```text
//! ingest_block() -> SpikeSnippetStore::process_block(&ap_block)
//!                   | per channel: detect negative-going threshold crossings
//!                   | collect pre_samples before + post_samples after crossing
//!                   | emit SpikeSnippet { samples, age_frames: 0 }
//! each render frame -> advance_frames() increments age; prunes stale snippets
//! pane_ui -> draw_spike_overlay(store, channels, ...)
//! ```
//!
//! ## Detection
//!
//! Threshold = −sigma × per-channel RMS estimate (exponential moving average).
//! Refractory period prevents double-counting the same spike.
//!
//! ## Rendering
//!
//! One `egui_plot::Plot` per selected channel stacked vertically.
//! Each snippet is drawn as a semi-transparent `Line`; alpha fades with age.

use std::collections::VecDeque;

use eframe::egui;
use egui_plot::{Line, Plot, PlotPoint, PlotPoints};
use kv_types::SampleBlock;

use crate::theme;

// ── Constants ────────────────────────────────────────────────────────

/// Default sigma multiplier for the detection threshold.
pub const DEFAULT_SIGMA: f32 = 4.0;
/// Default pre-crossing window, milliseconds.
pub const DEFAULT_PRE_MS: f32 = 0.5;
/// Default post-crossing window, milliseconds.
pub const DEFAULT_POST_MS: f32 = 1.5;
/// Default maximum snippets stored per channel.
pub const DEFAULT_MAX_SNIPPETS: usize = 50;
/// Render frames until a snippet becomes fully transparent.
const FADE_FRAMES: u32 = 180; // ~3 s at 60 fps

/// Default per-channel vertical scale (gain multiplier) for the overlay.
pub const DEFAULT_Y_SCALE: f32 = 1.0;

/// Convert a mean-absolute-value estimate to an equivalent RMS / standard
/// deviation for zero-mean noise: `RMS = mean(|s|) / sqrt(2/π)`. Applied so the
/// user-facing `sigma` control means true multiples of the noise standard
/// deviation, matching the waveform-view detector, instead of ~21% shallower.
const MEAN_ABS_TO_RMS: f32 = 1.253_314; // 1 / 0.797_884_6

// ── Selected channel ─────────────────────────────────────────────────

/// One channel selected for the Spike Overlay, with its own display scale.
///
/// `y_scale` is a per-channel vertical gain multiplier (1.0 = default).  Larger
/// values magnify low-amplitude channels so each can be inspected independently
/// — this is the per-channel "Y range" control the overlay exposes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpikeChannel {
    pub ch: usize,
    pub y_scale: f32,
}

impl SpikeChannel {
    pub fn new(ch: usize) -> Self {
        Self {
            ch,
            y_scale: DEFAULT_Y_SCALE,
        }
    }
}

// ── Snippet ──────────────────────────────────────────────────────────

/// Cache key for a snippet's rendered geometry. The geometry only changes when
/// the display window, the channel's vertical scale, or its lane position
/// changes — never per frame — so we rebuild only when this key changes.
#[derive(Clone, Copy, PartialEq, Eq)]
struct GeomKey {
    pre_bits: u32,
    post_bits: u32,
    y_scale_bits: u32,
    disp_pos: usize,
}

/// One captured threshold-crossing waveform.
pub struct SpikeSnippet {
    /// Normalised AP-filtered samples (pre + post, i16::MAX scale).
    pub samples: Vec<f32>,
    /// Frames elapsed since capture — drives fade-out alpha.
    pub age_frames: u32,
    /// Cached plot geometry, reused across frames while [`GeomKey`] is
    /// unchanged. egui is immediate-mode, so this is borrowed each frame to
    /// avoid rebuilding and re-allocating the point list when only the fade
    /// alpha (a colour, not geometry) changes (#4c).
    render_geom: Vec<PlotPoint>,
    render_key: Option<GeomKey>,
}

impl SpikeSnippet {
    fn new(samples: Vec<f32>) -> Self {
        Self {
            samples,
            age_frames: 0,
            render_geom: Vec::new(),
            render_key: None,
        }
    }

    /// Opacity: 1.0 when fresh, linearly decays to 0.0 at FADE_FRAMES.
    pub fn alpha(&self) -> f32 {
        (1.0 - self.age_frames as f32 / FADE_FRAMES as f32).clamp(0.0, 1.0)
    }

    /// Rebuild [`Self::render_geom`] only if the layout parameters changed.
    /// `y_scale` is applied here as a cheap multiplier so dragging the Y-scale
    /// control no longer forces a full per-frame geometry recompute.
    fn ensure_render_geom(
        &mut self,
        pre_ms: f32,
        post_ms: f32,
        ch_spacing: f64,
        y_scale: f32,
        disp_pos: usize,
    ) {
        let key = GeomKey {
            pre_bits: pre_ms.to_bits(),
            post_bits: post_ms.to_bits(),
            y_scale_bits: y_scale.to_bits(),
            disp_pos,
        };
        if self.render_key == Some(key) && !self.render_geom.is_empty() {
            return;
        }
        let total = self.samples.len();
        let x_left = -(pre_ms as f64);
        let x_right = post_ms as f64;
        let denom = total.saturating_sub(1).max(1) as f64;
        let gain = ch_spacing * 0.4 * y_scale as f64;
        let offset = disp_pos as f64 * ch_spacing;
        self.render_geom.clear();
        self.render_geom.reserve(total);
        for (i, &v) in self.samples.iter().enumerate() {
            let t_ms = x_left + (i as f64 / denom) * (x_right - x_left);
            let y = v as f64 * gain - offset;
            self.render_geom.push(PlotPoint::new(t_ms, y));
        }
        self.render_key = Some(key);
    }
}

// ── Per-channel buffer ───────────────────────────────────────────────

/// State machine for one channel's spike detection.
struct ChannelBuf {
    /// Ring of recent AP samples for the pre-crossing window.
    pre_ring: VecDeque<f32>,
    /// When Some: snapshot of pre-window taken at crossing; collecting post.
    pending: Option<(Vec<f32>, Vec<f32>)>, // (pre_snapshot, post_buf)
    /// Remaining refractory samples after last detection.
    refractory: usize,
    /// Previous sample value (for negative-going edge detection).
    prev: f32,
    /// Exponential moving average of absolute amplitude for RMS estimate.
    rms_ema: f32,
    /// Whether `rms_ema` has been seeded from real data. Until then the first
    /// sample seeds it directly, so the threshold is valid immediately instead
    /// of decaying from the initial guess for ~150 ms after start/reconfigure (L6).
    seeded: bool,
    /// Completed snippets.
    pub snippets: VecDeque<SpikeSnippet>,
}

impl ChannelBuf {
    fn new(pre_samples: usize) -> Self {
        let mut pre_ring = VecDeque::with_capacity(pre_samples + 1);
        pre_ring.extend(std::iter::repeat_n(0.0_f32, pre_samples));
        Self {
            pre_ring,
            pending: None,
            refractory: 0,
            prev: 0.0,
            rms_ema: 0.01, // provisional until seeded from the first real sample
            seeded: false,
            snippets: VecDeque::new(),
        }
    }

    /// Process a single normalised sample for this channel.
    fn push_sample(
        &mut self,
        s: f32,
        sigma: f32,
        pre_samples: usize,
        post_samples: usize,
        refractory_samples: usize,
        max_snippets: usize,
    ) {
        // Track an EMA of |s| (mean absolute value). Converted to a true RMS
        // sigma at threshold time via MEAN_ABS_TO_RMS. Seed from the first real
        // sample so the threshold is usable immediately (L6).
        if self.seeded {
            self.rms_ema = self.rms_ema * 0.9995 + s.abs() * 0.0005;
        } else {
            self.rms_ema = s.abs().max(1e-6);
            self.seeded = true;
        }

        // Keep pre-ring correctly sized
        while self.pre_ring.len() >= pre_samples.max(1) {
            self.pre_ring.pop_front();
        }
        self.pre_ring.push_back(s);

        // Refractory countdown
        if self.refractory > 0 {
            self.refractory -= 1;
            self.prev = s;
            // Still collect post-crossing samples if pending
            if let Some((_, ref mut post)) = self.pending {
                post.push(s);
                if post.len() >= post_samples {
                    self.emit_snippet(max_snippets);
                }
            }
            return;
        }

        // If currently collecting post-crossing samples
        if let Some((_, ref mut post)) = self.pending {
            post.push(s);
            if post.len() >= post_samples {
                self.emit_snippet(max_snippets);
            }
            self.prev = s;
            return;
        }

        // Detect negative-going threshold crossing. Convert the mean-abs EMA to
        // a true RMS sigma so `sigma` counts noise standard deviations.
        let noise_sigma = self.rms_ema * MEAN_ABS_TO_RMS;
        let thresh = -sigma * noise_sigma;
        if self.prev >= thresh && s < thresh {
            // Snapshot the pre-window now
            let pre_snapshot: Vec<f32> = self.pre_ring.iter().copied().collect();
            self.pending = Some((pre_snapshot, Vec::with_capacity(post_samples)));
            self.refractory = refractory_samples;
        }

        self.prev = s;
    }

    fn emit_snippet(&mut self, max_snippets: usize) {
        if let Some((pre, post)) = self.pending.take() {
            let mut samples = pre;
            samples.extend_from_slice(&post);
            if self.snippets.len() >= max_snippets {
                self.snippets.pop_front();
            }
            self.snippets.push_back(SpikeSnippet::new(samples));
        }
    }

    fn advance_frames(&mut self) {
        for s in &mut self.snippets {
            s.age_frames = s.age_frames.saturating_add(1);
        }
        // Prune fully transparent snippets
        while self
            .snippets
            .front()
            .is_some_and(|s| s.age_frames >= FADE_FRAMES)
        {
            self.snippets.pop_front();
        }
    }
}

// ── Store (one per KvApp) ─────────────────────────────────────────────

/// Holds per-channel snippet buffers and detection parameters.
pub struct SpikeSnippetStore {
    bufs: Vec<ChannelBuf>,
    /// Detection threshold = −sigma × per-channel RMS.
    pub sigma: f32,
    /// Pre-crossing samples to capture.
    pub pre_samples: usize,
    /// Post-crossing samples to capture.
    pub post_samples: usize,
    /// Max snippets retained per channel.
    pub max_snippets: usize,
    /// Sample rate (Hz) — needed for ms ↔ sample conversion.
    sample_rate: f64,
    /// Refractory period in samples (1 ms default).
    refractory_samples: usize,
}

impl SpikeSnippetStore {
    pub fn new(channel_count: usize, sample_rate: f64) -> Self {
        let pre_samples = ms_to_samples(DEFAULT_PRE_MS, sample_rate);
        let post_samples = ms_to_samples(DEFAULT_POST_MS, sample_rate);
        let refractory = ms_to_samples(1.0, sample_rate);
        Self {
            bufs: (0..channel_count)
                .map(|_| ChannelBuf::new(pre_samples))
                .collect(),
            sigma: DEFAULT_SIGMA,
            pre_samples,
            post_samples,
            max_snippets: DEFAULT_MAX_SNIPPETS,
            sample_rate,
            refractory_samples: refractory,
        }
    }

    /// Called when channel count or sample rate changes.
    pub fn reconfigure(&mut self, channel_count: usize, sample_rate: f64) {
        self.sample_rate = sample_rate;
        self.pre_samples = ms_to_samples(DEFAULT_PRE_MS, sample_rate);
        self.post_samples = ms_to_samples(DEFAULT_POST_MS, sample_rate);
        self.refractory_samples = ms_to_samples(1.0, sample_rate);
        self.bufs = (0..channel_count)
            .map(|_| ChannelBuf::new(self.pre_samples))
            .collect();
    }

    /// Update pre/post window sizes from ms values (rebuilds buffers).
    pub fn set_window_ms(&mut self, pre_ms: f32, post_ms: f32) {
        let new_pre = ms_to_samples(pre_ms, self.sample_rate);
        let new_post = ms_to_samples(post_ms, self.sample_rate);
        if new_pre != self.pre_samples || new_post != self.post_samples {
            self.pre_samples = new_pre;
            self.post_samples = new_post;
            for buf in &mut self.bufs {
                buf.pre_ring = {
                    let mut r = VecDeque::with_capacity(new_pre + 1);
                    r.extend(std::iter::repeat_n(0.0_f32, new_pre));
                    r
                };
                buf.pending = None;
            }
        }
    }

    /// Process one AP-filtered block (interleaved i16 samples).
    pub fn process_block(&mut self, block: &SampleBlock) {
        let ch = block.channel_count;
        let spc = block.samples_per_channel;
        // Guard the interleaved-index arithmetic below against a short/malformed
        // block rather than panicking on out-of-bounds (I2).
        if block.data.len() < ch * spc {
            return;
        }
        if self.bufs.len() != ch {
            self.reconfigure(ch, block.sample_rate);
        }
        let sigma = self.sigma;
        let pre = self.pre_samples;
        let post = self.post_samples;
        let refrac = self.refractory_samples;
        let max = self.max_snippets;
        let scale = i16::MAX as f32;

        for s in 0..spc {
            for c in 0..ch {
                let v = block.data[s * ch + c] as f32 / scale;
                self.bufs[c].push_sample(v, sigma, pre, post, refrac, max);
            }
        }
    }

    /// Increment age and prune stale snippets — call once per render frame.
    pub fn advance_frames(&mut self) {
        for buf in &mut self.bufs {
            buf.advance_frames();
        }
    }

    /// Return reference to snippets for a specific physical channel. An
    /// out-of-range channel yields an empty deque — never another channel's
    /// snippets (previously it clamped to the last buffer, so a stale high
    /// channel would silently render the last channel's data under a wrong
    /// label after the channel count dropped).
    pub fn snippets_for(&self, ch: usize) -> &VecDeque<SpikeSnippet> {
        static EMPTY: VecDeque<SpikeSnippet> = VecDeque::new();
        match self.bufs.get(ch) {
            Some(buf) => &buf.snippets,
            None => &EMPTY,
        }
    }

    /// Mutable snippets for a channel — lets the renderer refresh each
    /// snippet's cached geometry in place. Returns `None` for an out-of-range
    /// channel (rather than clamping to the last buffer).
    pub fn snippets_for_mut(&mut self, ch: usize) -> Option<&mut VecDeque<SpikeSnippet>> {
        self.bufs.get_mut(ch).map(|b| &mut b.snippets)
    }

    pub fn channel_count(&self) -> usize {
        self.bufs.len()
    }
    pub fn pre_ms(&self) -> f32 {
        samples_to_ms(self.pre_samples, self.sample_rate)
    }
    pub fn post_ms(&self) -> f32 {
        samples_to_ms(self.post_samples, self.sample_rate)
    }
}

// ── Renderer ──────────────────────────────────────────────────────────

/// Render snippet overlays for the given `selected_channels`.
///
/// Each channel gets its own row in a stacked plot layout.
/// Snippets are drawn as overlaid semi-transparent lines fading with age.
pub fn draw_spike_overlay(
    ui: &mut egui::Ui,
    store: &mut SpikeSnippetStore,
    channels: &[SpikeChannel],
    show_grid: bool,
    tile_id_salt: usize,
) {
    if channels.is_empty() || store.channel_count() == 0 {
        ui.centered_and_justified(|ui| {
            ui.label(
                egui::RichText::new("No channels selected.\nClick a channel below to monitor it.")
                    .size(12.0)
                    .color(theme::TEXT_DIM),
            );
        });
        return;
    }

    let pre_ms = store.pre_ms();
    let post_ms = store.post_ms();
    let x_left = -(pre_ms as f64);
    let x_right = post_ms as f64;
    let ch_spacing = 2.5_f64;
    let n = channels.len();
    let y_min = -(n as f64) * ch_spacing + ch_spacing * 0.5;
    let y_max = ch_spacing * 0.5;

    // Phase 1: refresh each visible snippet's cached geometry in place. This is
    // a no-op unless the window / y-scale / lane position changed, so in steady
    // state (only the fade alpha changing) nothing is rebuilt or reallocated.
    for (disp_pos, sc) in channels.iter().enumerate() {
        let y_scale = sc.y_scale;
        let Some(snips) = store.snippets_for_mut(sc.ch) else {
            continue;
        };
        for snippet in snips {
            if snippet.alpha() < 0.02 {
                continue;
            }
            snippet.ensure_render_geom(pre_ms, post_ms, ch_spacing, y_scale, disp_pos);
        }
    }

    // Phase 2: borrow the cached geometry (no per-frame allocation) for drawing.
    let mut all_lines: Vec<(usize, f32, &[PlotPoint])> = Vec::new(); // (disp_pos, alpha, pts)
    for (disp_pos, sc) in channels.iter().enumerate() {
        for snippet in store.snippets_for(sc.ch) {
            let alpha = snippet.alpha();
            if alpha < 0.02 {
                continue;
            }
            all_lines.push((disp_pos, alpha, snippet.render_geom.as_slice()));
        }
    }

    // Y-axis label formatter
    let channels_for_fmt: Vec<usize> = channels.iter().map(|c| c.ch).collect();
    let spacing_fmt = ch_spacing;
    let y_fmt = move |mark: egui_plot::GridMark, _: &std::ops::RangeInclusive<f64>| {
        let disp = (-mark.value / spacing_fmt).round() as i64;
        if disp >= 0 && (disp as usize) < channels_for_fmt.len() {
            format!("CH{}", channels_for_fmt[disp as usize])
        } else {
            String::new()
        }
    };

    let plot_id = format!("spike_overlay_{tile_id_salt}");
    let plot = Plot::new(plot_id)
        .height(ui.available_height())
        .width(ui.available_width())
        .show_axes([true, true])
        .show_grid(show_grid)
        .allow_drag(false)
        .allow_zoom(false)
        .allow_scroll(false)
        .allow_boxed_zoom(false)
        .auto_bounds(egui::Vec2b::new(false, false))
        .show_x(false)
        .show_y(false)
        .y_axis_formatter(y_fmt)
        .set_margin_fraction(egui::vec2(0.0, 0.01));

    // Zero-reference lines (drawn as vertical axis marker at t=0)
    plot.show(ui, |plot_ui| {
        plot_ui.set_plot_bounds(egui_plot::PlotBounds::from_min_max(
            [x_left, y_min],
            [x_right, y_max],
        ));

        // t=0 cursor line
        plot_ui.line(
            Line::new(PlotPoints::from(vec![[0.0, y_min], [0.0, y_max]]))
                .color(egui::Color32::from_rgba_unmultiplied(150, 150, 150, 60))
                .width(1.0_f32)
                .name(""),
        );

        // Zero baselines
        for disp_pos in 0..n {
            let y_off = -(disp_pos as f64) * ch_spacing;
            plot_ui.line(
                Line::new(PlotPoints::from(vec![[x_left, y_off], [x_right, y_off]]))
                    .color(theme::GRID_ZERO_LINE)
                    .width(0.4_f32)
                    .name(""),
            );
        }

        // Snippet lines with fade-out. Geometry is borrowed from each snippet's
        // cache; only the colour (alpha) is recomputed per frame.
        for (disp_pos, alpha, pts) in all_lines {
            let base = theme::channel_color(channels[disp_pos].ch);
            let a = (alpha * 220.0) as u8;
            let color = egui::Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), a);
            plot_ui.line(
                Line::new(PlotPoints::Borrowed(pts))
                    .color(color)
                    .width(1.0_f32)
                    .name(""),
            );
        }
    });
}

// ── Helpers ───────────────────────────────────────────────────────────

fn ms_to_samples(ms: f32, sample_rate: f64) -> usize {
    ((ms as f64 / 1000.0) * sample_rate).round().max(1.0) as usize
}

fn samples_to_ms(samples: usize, sample_rate: f64) -> f32 {
    (samples as f64 / sample_rate * 1000.0) as f32
}
