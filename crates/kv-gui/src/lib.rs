//! Keyvast GUI — professional acquisition monitoring interface.
//!
//! Provides multi-channel waveform visualization, real-time statistics,
//! buffer health monitoring, and recording controls using `egui` / `eframe`.

mod app;
mod panels;
mod preview;
mod theme;
mod waveform;

pub use app::KvApp;
