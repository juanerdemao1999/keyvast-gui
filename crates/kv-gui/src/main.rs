//! Keyvast GUI entry point.

fn main() -> eframe::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1200.0, 800.0])
            // Floor wide enough for the single-row toolbar to lay out without
            // clipping the right-aligned REC/LIVE status pill + clock (C6).
            .with_min_inner_size([1100.0, 640.0])
            .with_title("Keyvast Acquisition"),
        ..Default::default()
    };

    eframe::run_native(
        "Keyvast",
        options,
        Box::new(|cc| Ok(Box::new(kv_gui::KvApp::new(cc)))),
    )
}
