use super::*;

impl KvApp {
    /// Convert a .kvraw recording to the selected export format on a
    /// background thread, writing the output next to the source file.
    pub(crate) fn start_export(&mut self, source: std::path::PathBuf) {
        let format = self.export_format;
        let (tx, rx) = std::sync::mpsc::channel();
        self.export_rx = Some(rx);
        self.export_status = None;
        std::thread::spawn(move || {
            let _ = tx.send(export_kvraw(&source, format));
        });
    }

    /// Drain the result of the background .kvraw export, if any.
    pub(crate) fn poll_export(&mut self) {
        let Some(rx) = self.export_rx.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok(Ok(path)) => {
                self.toasts.success(format!(
                    "Exported \u{2192} {}",
                    path.file_name().and_then(|n| n.to_str()).unwrap_or("file")
                ));
                self.export_status = Some(format!("Exported → {}", path.display()));
                self.export_rx = None;
            }
            Ok(Err(e)) => {
                self.toasts.error(format!("Export failed: {e}"));
                self.export_status = Some(format!("Export failed: {e}"));
                self.export_rx = None;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.toasts.error("Export thread exited unexpectedly");
                self.export_status = Some("export thread exited unexpectedly".to_string());
                self.export_rx = None;
            }
        }
    }
}

/// Read an entire .kvraw file and export it in the requested format.
/// Returns the output path on success.
fn export_kvraw(
    source: &std::path::Path,
    format: kv_recorder::export_formats::ExportFormat,
) -> Result<std::path::PathBuf, String> {
    use std::cell::RefCell;
    use std::rc::Rc;

    use kv_recorder::KvrawReader;
    use kv_recorder::export_formats::{self, ExportFormat, ExportHeader, RhdFilterConfig};

    // Native format needs no conversion — just copy the .kvraw alongside.
    if format.is_native() {
        let output = source.with_extension("copy.kvraw");
        std::fs::copy(source, &output).map_err(|e| e.to_string())?;
        return Ok(output);
    }

    let mut reader = KvrawReader::open(source).map_err(|e| e.to_string())?;
    let meta = reader.metadata().clone();
    if meta.channel_count == 0 {
        return Err("kvraw file has no channels".to_string());
    }
    let total_frames = reader.total_frames();
    if total_frames == 0 {
        return Err("no data to export".to_string());
    }

    let header = ExportHeader {
        sample_rate: meta.sample_rate,
        channel_count: meta.channel_count,
        filter: RhdFilterConfig::default(),
    };
    let notes = format!("exported from {}", source.display());

    // Stream blocks straight from disk into the exporter. Reading happens lazily
    // inside the iterator so the whole recording is never held in memory; a read
    // failure is captured and surfaced after the exporter returns.
    const FRAMES_PER_CHUNK: usize = 30_000;
    let read_err: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let read_err_sink = Rc::clone(&read_err);
    let channel_count = meta.channel_count;
    let device_id = meta.device_id.clone();
    let sample_rate = meta.sample_rate;
    let mut frame: u64 = 0;
    let mut packet_id: u64 = 0;
    let blocks = std::iter::from_fn(move || {
        if frame >= total_frames {
            return None;
        }
        let want = FRAMES_PER_CHUNK.min((total_frames - frame) as usize);
        match reader.read_frames(frame, want) {
            Ok(data) => {
                if data.is_empty() {
                    return None;
                }
                let frames_read = data.len() / channel_count;
                let block = SampleBlock {
                    device_id: device_id.clone(),
                    stream_id: 0,
                    packet_id,
                    timestamp_start: frame,
                    sample_rate,
                    channel_count,
                    samples_per_channel: frames_read,
                    ttl_bits: 0,
                    data,
                    aux_data: None,
                    board_adc_data: None,
                    ttl_in_per_sample: None,
                    ttl_out_per_sample: None,
                };
                packet_id += 1;
                frame += frames_read as u64;
                Some(block)
            }
            Err(e) => {
                *read_err_sink.borrow_mut() = Some(e.to_string());
                None
            }
        }
    });

    let result = match format {
        // Native is short-circuited above before any frames are read.
        ExportFormat::KeyvastNative => unreachable!("native format handled before frame read"),
        ExportFormat::IntanRhd => {
            let output = source.with_extension(format.extension());
            export_formats::export_intan_rhd_streaming(&output, header, blocks, &notes)
        }
        ExportFormat::FlatBinary => {
            // Flat binary writes recording.bin + recording.meta.json into a directory.
            let output_dir = source.with_extension("export");
            export_formats::export_flat_binary_streaming(&output_dir, header, blocks, &notes)
        }
    };

    if let Some(e) = read_err.borrow_mut().take() {
        return Err(e);
    }
    result.map_err(|e| e.to_string())
}
