//! Keyvast GUI — professional acquisition monitoring interface.
//!
//! Provides multi-channel waveform visualization, real-time statistics,
//! buffer health monitoring, and recording controls using `egui` / `eframe`.
//!
//! Includes a **Demo mode** that auto-generates realistic in-vivo neural
//! signals (spikes, LFP, bursts) for demonstration without hardware.

mod app;
mod demo;
mod disp_ring;
mod dsp;
mod live_pipeline;
mod panels;
mod preview;
mod theme;
mod waveform;

pub use app::KvApp;
