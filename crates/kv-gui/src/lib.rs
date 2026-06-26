//! Keyvast GUI — professional acquisition monitoring interface.
//!
//! Provides multi-channel waveform visualization, real-time statistics,
//! buffer health monitoring, and recording controls using `egui` / `eframe`.
//!
//! Includes a **Demo mode** that auto-generates realistic in-vivo neural
//! signals (spikes, LFP, bursts) for demonstration without hardware.

mod app;
mod channel_map;
mod channel_select;
mod config_persist;
mod demo;
mod diskspace;
mod disp_ring;
mod dsp;
mod fft_panel;
mod impedance_panel;
mod live_pipeline;
mod multiview;
mod panels;
mod panic_guard;
mod playback;
mod preview;
mod remote_api;
mod spike_overlay;
mod theme;
mod toast;
mod trigger;
mod waveform;

pub use app::KvApp;
