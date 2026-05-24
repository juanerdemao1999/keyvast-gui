//! Waveform rendering for egui.

use eframe::egui;
use kv_types::SampleBlock;

/// Number of channels to display simultaneously.
const DEFAULT_VISIBLE_CHANNELS: usize = 16;

/// Draw a multi-channel waveform panel from the given SampleBlock.
pub fn draw_waveform_panel(ui: &mut egui::Ui, block: &SampleBlock, visible_channels: usize) {
    let channels = visible_channels.min(block.channel_count);
    if channels == 0 || block.samples_per_channel == 0 {
        ui.label("No data");
        return;
    }

    let available = ui.available_size();
    let channel_height = (available.y / channels as f32).max(20.0);

    for ch in 0..channels {
        let (response, painter) = ui.allocate_painter(
            egui::vec2(available.x, channel_height),
            egui::Sense::hover(),
        );
        let rect = response.rect;

        // Background
        let bg = if ch % 2 == 0 {
            egui::Color32::from_gray(30)
        } else {
            egui::Color32::from_gray(25)
        };
        painter.rect_filled(rect, 0.0, bg);

        // Channel label
        painter.text(
            rect.left_top() + egui::vec2(4.0, 2.0),
            egui::Align2::LEFT_TOP,
            format!("CH{ch}"),
            egui::FontId::monospace(10.0),
            egui::Color32::from_gray(120),
        );

        // Draw waveform trace
        let samples_count = block.samples_per_channel;
        let x_step = rect.width() / samples_count.max(1) as f32;
        let mid_y = rect.center().y;
        let half_h = (channel_height * 0.4).max(1.0);

        let points: Vec<egui::Pos2> = (0..samples_count)
            .map(|s| {
                let idx = s * block.channel_count + ch;
                let value = if idx < block.data.len() {
                    block.data[idx] as f32 / i16::MAX as f32
                } else {
                    0.0
                };
                egui::pos2(rect.left() + s as f32 * x_step, mid_y - value * half_h)
            })
            .collect();

        if points.len() >= 2 {
            let stroke = egui::Stroke::new(1.0, channel_color(ch));
            for window in points.windows(2) {
                painter.line_segment([window[0], window[1]], stroke);
            }
        }
    }
}

/// Draw a status panel showing acquisition health metrics.
pub fn draw_status_panel(
    ui: &mut egui::Ui,
    block: &SampleBlock,
    block_count: u64,
    visible_channels: usize,
) {
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(format!("Blocks: {block_count}"))
                .monospace()
                .color(egui::Color32::LIGHT_GREEN),
        );
        ui.separator();
        ui.label(
            egui::RichText::new(format!("Packet: {}", block.packet_id))
                .monospace()
                .color(egui::Color32::LIGHT_BLUE),
        );
        ui.separator();
        ui.label(
            egui::RichText::new(format!(
                "{}ch x {:.0}Hz",
                block.channel_count, block.sample_rate
            ))
            .monospace(),
        );
        ui.separator();
        ui.label(
            egui::RichText::new(format!("Visible: {visible_channels}ch"))
                .monospace()
                .color(egui::Color32::YELLOW),
        );
        ui.separator();
        ui.label(
            egui::RichText::new(format!("TTL: {:#06x}", block.ttl_bits))
                .monospace()
                .color(egui::Color32::from_rgb(200, 100, 200)),
        );
    });
}

/// Per-channel color palette.
fn channel_color(channel: usize) -> egui::Color32 {
    const PALETTE: &[egui::Color32] = &[
        egui::Color32::from_rgb(100, 200, 100),
        egui::Color32::from_rgb(100, 150, 255),
        egui::Color32::from_rgb(255, 150, 100),
        egui::Color32::from_rgb(200, 200, 100),
        egui::Color32::from_rgb(200, 100, 200),
        egui::Color32::from_rgb(100, 200, 200),
        egui::Color32::from_rgb(255, 100, 100),
        egui::Color32::from_rgb(150, 200, 255),
    ];
    PALETTE[channel % PALETTE.len()]
}

/// Default number of visible channels for the initial view.
pub fn default_visible_channels() -> usize {
    DEFAULT_VISIBLE_CHANNELS
}
