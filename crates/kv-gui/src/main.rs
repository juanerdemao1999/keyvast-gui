//! Keyvast GUI entry point.

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1200.0, 800.0])
            .with_title("Keyvast Acquisition"),
        ..Default::default()
    };

    eframe::run_native(
        "Keyvast",
        options,
        Box::new(|cc| Ok(Box::new(kv_gui::KvApp::new(cc)))),
    )
}
