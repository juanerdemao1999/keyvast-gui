//! Keyvast GUI — engineering display for real-time acquisition monitoring.
//!
//! Provides waveform visualization, buffer health, and acquisition status
//! using `egui` / `eframe`. Connects to the pipeline's preview consumer
//! via an `mpsc` channel.

mod app;
mod preview;
mod waveform;

pub use app::KvApp;
pub use preview::{PreviewCommand, PreviewHandle, start_preview};
