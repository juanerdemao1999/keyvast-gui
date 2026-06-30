//! Top-level command execution: dispatch plus the simulator, benchmark and
//! RHD smoke-test runners.

#![allow(clippy::wildcard_imports)]

use crate::report::*;
use crate::*;

pub fn run_command(command: CliCommand) -> Result<CommandResult, CliError> {
    match command {
        CliCommand::SimulatorRecord(options) => {
            run_simulator_recording(options).map(CommandResult::Record)
        }
        CliCommand::SimulatorPipeline(options) => {
            run_simulator_pipeline(options).map(CommandResult::Pipeline)
        }
        CliCommand::SimulatorStream(options) => {
            run_simulator_stream(options).map(CommandResult::Stream)
        }
        CliCommand::Benchmark(options) => run_benchmark(options).map(CommandResult::Benchmark),
        CliCommand::RhdSmoke(options) => run_rhd_smoke(options).map(CommandResult::RhdSmoke),
    }
}

pub fn run_simulator_recording(
    options: SimulatorRecordingOptions,
) -> Result<SimulatorRecordingResult, CliError> {
    if options.blocks == 0 {
        return Err(CliError::InvalidBlockCount { blocks: 0 });
    }

    let simulator_config = SimulatorConfig {
        drop_packet_ids: options.drop_packet_ids,
        ..SimulatorConfig::default()
    };
    let device_config = simulator_config.device.clone();
    let mut simulator = SimulatorBackend::new(simulator_config)?;

    let acquisition = match run_fixed_blocks(&device_config, options.blocks, &mut || {
        if is_cancelled() {
            return Err(SimulatorReadError("cancelled by Ctrl-C".to_string()));
        }
        simulator
            .next_block()
            .map_err(|e| SimulatorReadError(e.to_string()))
    }) {
        Ok(acquisition) => acquisition,
        Err(AcquisitionRunError::BackendRead {
            summary,
            message,
            blocks,
        }) => {
            // DA14: persist whatever was acquired before the failure instead of
            // discarding the entire run.
            persist_partial_simulator_recording(&options.output_dir, &blocks, &summary)?;
            return Err(CliError::Acquisition(Box::new(
                AcquisitionRunError::BackendRead {
                    summary,
                    message,
                    blocks: Vec::new(),
                },
            )));
        }
        Err(other) => return Err(other.into()),
    };
    let recording = write_recording(&options.output_dir, &acquisition.blocks)?;
    write_integrity_summary(&options.output_dir, &acquisition.integrity.summary)?;
    write_log_file(
        &options.output_dir,
        &simulator_recording_log_lines(&acquisition.integrity),
    )?;
    let events = simulator_recording_events(
        &acquisition.integrity,
        ttl_change_events(&acquisition.blocks),
    );
    write_events_csv(&options.output_dir, &events)?;
    let benchmark =
        simulator_benchmark_summary(&acquisition.summary, &recording, &acquisition.integrity);
    write_benchmark_summary(&options.output_dir, &benchmark)?;

    Ok(SimulatorRecordingResult {
        acquisition: acquisition.summary,
        recording,
        integrity: acquisition.integrity,
    })
}

pub fn run_simulator_pipeline(
    options: SimulatorPipelineOptions,
) -> Result<SimulatorPipelineResult, CliError> {
    use kv_core::pipeline::run_threaded_pipeline;

    if options.blocks == 0 {
        return Err(CliError::InvalidBlockCount { blocks: 0 });
    }

    let simulator_config = SimulatorConfig {
        drop_packet_ids: options.drop_packet_ids,
        ..SimulatorConfig::default()
    };
    let device_config = simulator_config.device.clone();
    let simulator = SimulatorBackend::new(simulator_config)?;

    let pipeline_config = PipelineConfig {
        device: device_config,
        requested_blocks: options.blocks,
        recorder_capacity_blocks: options.recorder_capacity_blocks,
        preview_capacity_blocks: options.preview_capacity_blocks,
    };

    let source = {
        let mut sim = simulator;
        move || {
            if is_cancelled() {
                return Err("cancelled by Ctrl-C".to_string());
            }
            sim.next_block().map_err(|e| e.to_string())
        }
    };

    let pipeline_result = run_threaded_pipeline(&pipeline_config, source)?;

    let recording = write_recording(&options.output_dir, &pipeline_result.recorded_blocks)?;
    write_integrity_summary(&options.output_dir, &pipeline_result.integrity.summary)?;
    write_log_file(
        &options.output_dir,
        &simulator_recording_log_lines(&pipeline_result.integrity),
    )?;
    let events = simulator_recording_events(
        &pipeline_result.integrity,
        ttl_change_events(&pipeline_result.recorded_blocks),
    );
    write_events_csv(&options.output_dir, &events)?;

    let benchmark = pipeline_benchmark_summary(&pipeline_result, &recording);
    write_benchmark_summary(&options.output_dir, &benchmark)?;

    Ok(SimulatorPipelineResult {
        recording,
        integrity: pipeline_result.integrity,
        timing: pipeline_result.timing,
        recorder_dropped_blocks: pipeline_result.recorder_status.dropped_blocks,
        preview_dropped_blocks: pipeline_result.preview_status.dropped_blocks,
    })
}

pub fn run_simulator_stream(
    options: SimulatorPipelineOptions,
) -> Result<SimulatorStreamResult, CliError> {
    if options.blocks == 0 {
        return Err(CliError::InvalidBlockCount { blocks: 0 });
    }

    let simulator_config = SimulatorConfig {
        drop_packet_ids: options.drop_packet_ids,
        ..SimulatorConfig::default()
    };
    let device_config = simulator_config.device.clone();
    let simulator = SimulatorBackend::new(simulator_config)?;

    let streaming_config = StreamingPipelineConfig {
        device: device_config,
        requested_blocks: options.blocks,
        output_dir: options.output_dir.clone(),
        recorder_capacity_blocks: options.recorder_capacity_blocks,
        preview_capacity_blocks: options.preview_capacity_blocks,
    };

    let source = {
        let mut sim = simulator;
        move || {
            if is_cancelled() {
                return Err("cancelled by Ctrl-C".to_string());
            }
            sim.next_block().map_err(|e| e.to_string())
        }
    };

    let result = run_streaming_pipeline(&streaming_config, source)?;

    write_integrity_summary(&options.output_dir, &result.integrity.summary)?;
    write_log_file(
        &options.output_dir,
        &simulator_recording_log_lines(&result.integrity),
    )?;
    let events = simulator_recording_events(&result.integrity, Vec::new());
    write_events_csv(&options.output_dir, &events)?;

    let benchmark = streaming_benchmark_summary(&result, &streaming_config.device, None, None);
    write_benchmark_summary(&options.output_dir, &benchmark)?;

    Ok(SimulatorStreamResult {
        recording: result.recording,
        integrity: result.integrity,
        timing: result.timing,
        recorder_dropped_blocks: result.recorder_status.dropped_blocks,
        preview_dropped_blocks: result.preview_status.dropped_blocks,
        max_write_latency_us: result.max_write_latency_us,
    })
}

/// Computes the number of blocks needed to cover `duration_seconds` given
/// `sample_rate` and `samples_per_packet`.
pub fn blocks_for_duration(
    duration_seconds: f64,
    sample_rate: f64,
    samples_per_packet: usize,
) -> usize {
    if samples_per_packet == 0 || sample_rate <= 0.0 || duration_seconds <= 0.0 {
        return 0;
    }
    let seconds_per_block = samples_per_packet as f64 / sample_rate;
    (duration_seconds / seconds_per_block).ceil() as usize
}

pub fn run_benchmark(options: BenchmarkOptions) -> Result<BenchmarkResult, CliError> {
    if !options.duration_seconds.is_finite() || options.duration_seconds <= 0.0 {
        return Err(CliError::InvalidDuration {
            seconds: options.duration_seconds,
        });
    }

    let block_count = blocks_for_duration(
        options.duration_seconds,
        options.sample_rate,
        options.samples_per_packet,
    );
    if block_count == 0 {
        return Err(CliError::InvalidBlockCount { blocks: 0 });
    }

    let mut device = DeviceConfig::simulator_default();
    device.channel_count = options.channel_count;
    device.enabled_channels = (0..options.channel_count).collect();
    device.sample_rate = options.sample_rate;
    device.samples_per_packet = options.samples_per_packet;

    let simulator_config = SimulatorConfig {
        device: device.clone(),
        drop_packet_ids: options.drop_packet_ids,
        ..SimulatorConfig::default()
    };
    let simulator = SimulatorBackend::new(simulator_config)?;

    let streaming_config = StreamingPipelineConfig {
        device: device.clone(),
        requested_blocks: block_count,
        output_dir: options.output_dir.clone(),
        recorder_capacity_blocks: options.recorder_capacity_blocks,
        preview_capacity_blocks: options.preview_capacity_blocks,
    };

    let source = {
        let mut sim = simulator;
        move || {
            if is_cancelled() {
                return Err("cancelled by Ctrl-C".to_string());
            }
            sim.next_block().map_err(|e| e.to_string())
        }
    };

    let metrics_collector = ProcessMetricsCollector::start();
    let result = run_streaming_pipeline(&streaming_config, source)?;
    let process_metrics = metrics_collector.finish(result.timing.wall_clock_seconds);

    write_integrity_summary(&options.output_dir, &result.integrity.summary)?;
    write_log_file(
        &options.output_dir,
        &simulator_recording_log_lines(&result.integrity),
    )?;
    let events = simulator_recording_events(&result.integrity, Vec::new());
    write_events_csv(&options.output_dir, &events)?;

    let benchmark = streaming_benchmark_summary(
        &result,
        &device,
        process_metrics.as_ref(),
        Some(options.duration_seconds),
    );
    write_benchmark_summary(&options.output_dir, &benchmark)?;

    Ok(BenchmarkResult {
        recording: result.recording,
        integrity: result.integrity,
        timing: result.timing,
        recorder_dropped_blocks: result.recorder_status.dropped_blocks,
        preview_dropped_blocks: result.preview_status.dropped_blocks,
        max_write_latency_us: result.max_write_latency_us,
        requested_duration_seconds: options.duration_seconds,
        computed_block_count: block_count,
        process_metrics,
    })
}

pub fn run_rhd_smoke(options: RhdSmokeOptions) -> Result<RhdSmokeResult, CliError> {
    if options.blocks == 0 {
        return Err(CliError::InvalidBlockCount { blocks: 0 });
    }

    let data_config = RhythmDataConfig {
        device_id: DEFAULT_RHD_DEVICE_ID.to_string(),
        stream_id: 0,
        enabled_streams: options.enabled_streams,
        sample_rate: options.sample_rate,
        samples_per_block: SAMPLES_PER_USB_BLOCK,
    };
    let device_config = data_config
        .device_config()
        .map_err(RhdReadError::InvalidConfig)?;

    let hardware = options.raw_input.is_none();
    let acquisition_result = if let Some(raw_input) = &options.raw_input {
        let raw = fs::read(raw_input).map_err(|source| CliError::Io {
            path: raw_input.clone(),
            source,
        })?;
        let block_bytes =
            bytes_per_block(data_config.enabled_streams, data_config.samples_per_block)
                .map_err(RhdReadError::InvalidConfig)?;
        let expected_bytes = block_bytes.saturating_mul(options.blocks);
        if raw.len() < expected_bytes {
            return Err(CliError::RawInputTooShort {
                path: raw_input.clone(),
                expected_bytes,
                observed_bytes: raw.len(),
            });
        }

        let mut next_packet_id = 0_u64;
        run_fixed_blocks(&device_config, options.blocks, &mut || {
            if is_cancelled() {
                return Err(RhdReadError::Cancelled);
            }
            // Saturating arithmetic avoids an overflow panic; the slice bounds
            // are already guaranteed by the `expected_bytes` check above.
            let start = (next_packet_id as usize).saturating_mul(block_bytes);
            let end = start.saturating_add(block_bytes);
            let block = parse_rhythm_data_block(next_packet_id, &raw[start..end], &data_config)
                .map_err(RhdReadError::Parse)?;
            next_packet_id = next_packet_id.saturating_add(1);
            Ok::<_, RhdReadError>(block)
        })
    } else {
        let mut backend = RhdHardwareBackend::open(RhdHardwareOptions {
            bitfile_path: options.bitfile_path.clone(),
            frontpanel_dll_path: options.frontpanel_dll_path.clone(),
            serial: options.serial.clone(),
            data: data_config.clone(),
            cable_length_meters: options.cable_length_meters,
        })?;

        run_fixed_blocks(&device_config, options.blocks, &mut || {
            if is_cancelled() {
                return Err(RhdReadError::Cancelled);
            }
            backend.read_block()
        })
    };

    let acquisition = match acquisition_result {
        Ok(acquisition) => acquisition,
        Err(AcquisitionRunError::BackendRead {
            summary,
            message,
            blocks,
        }) => {
            // DA14: finalize a partial recording instead of discarding every
            // block acquired before the hardware/parse read failed.
            persist_partial_rhd_recording(&options.output_dir, &blocks, &summary, hardware)?;
            return Err(CliError::Acquisition(Box::new(
                AcquisitionRunError::BackendRead {
                    summary,
                    message,
                    blocks: Vec::new(),
                },
            )));
        }
        Err(other) => return Err(other.into()),
    };

    let recording =
        write_recording_with_backend(&options.output_dir, &acquisition.blocks, "rhd-frontpanel")?;
    write_integrity_summary(&options.output_dir, &acquisition.integrity.summary)?;
    write_log_file(
        &options.output_dir,
        &rhd_smoke_log_lines(&acquisition.integrity, options.raw_input.is_none()),
    )?;
    write_events_csv(
        &options.output_dir,
        &rhd_smoke_events(&acquisition.integrity),
    )?;
    write_benchmark_summary(
        &options.output_dir,
        &rhd_smoke_benchmark_summary(
            &acquisition.summary,
            &recording,
            &acquisition.integrity,
            options.raw_input.is_none(),
        ),
    )?;

    Ok(RhdSmokeResult {
        acquisition: acquisition.summary,
        recording,
        integrity: acquisition.integrity,
        hardware: options.raw_input.is_none(),
    })
}

/// Warning line prepended to the log of a recording that ended early because
/// the backend read failed (DA14).
const PARTIAL_RECORDING_WARNING: &str =
    "[WARN] acquisition ended early: backend read failed; partial recording finalized";

/// Finalize a partial simulator recording from the blocks acquired before a
/// mid-run read failure (DA14).  Does nothing when no blocks were acquired.
fn persist_partial_simulator_recording(
    output_dir: &PathBuf,
    blocks: &[SampleBlock],
    summary: &AcquisitionRunSummary,
) -> Result<(), CliError> {
    if blocks.is_empty() {
        return Ok(());
    }
    let recording = write_recording(output_dir, blocks)?;
    if let Ok(integrity) = check_blocks(blocks) {
        write_integrity_summary(output_dir, &integrity.summary)?;
        let mut lines = simulator_recording_log_lines(&integrity);
        lines.insert(0, PARTIAL_RECORDING_WARNING.to_string());
        write_log_file(output_dir, &lines)?;
        write_events_csv(
            output_dir,
            &simulator_recording_events(&integrity, Vec::new()),
        )?;
        let benchmark = simulator_benchmark_summary(summary, &recording, &integrity);
        write_benchmark_summary(output_dir, &benchmark)?;
    }
    Ok(())
}

/// Finalize a partial RHD smoke recording from the blocks acquired before a
/// mid-run read failure (DA14).  Does nothing when no blocks were acquired.
fn persist_partial_rhd_recording(
    output_dir: &PathBuf,
    blocks: &[SampleBlock],
    summary: &AcquisitionRunSummary,
    hardware: bool,
) -> Result<(), CliError> {
    if blocks.is_empty() {
        return Ok(());
    }
    let recording = write_recording_with_backend(output_dir, blocks, "rhd-frontpanel")?;
    if let Ok(integrity) = check_blocks(blocks) {
        write_integrity_summary(output_dir, &integrity.summary)?;
        let mut lines = rhd_smoke_log_lines(&integrity, hardware);
        lines.insert(0, PARTIAL_RECORDING_WARNING.to_string());
        write_log_file(output_dir, &lines)?;
        write_events_csv(output_dir, &rhd_smoke_events(&integrity))?;
        let benchmark = rhd_smoke_benchmark_summary(summary, &recording, &integrity, hardware);
        write_benchmark_summary(output_dir, &benchmark)?;
    }
    Ok(())
}

#[derive(Debug)]
struct SimulatorReadError(String);

impl fmt::Display for SimulatorReadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_output_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("kv-da14-{label}-{nanos}"))
    }

    /// DA14: a backend read failing mid-run must still finalize the blocks
    /// acquired before the failure rather than discarding the whole run.
    #[test]
    fn partial_simulator_recording_is_finalized_on_mid_run_failure() {
        let output_dir = unique_output_dir("partial");
        let config = SimulatorConfig::default();
        let device_config = config.device.clone();
        let mut simulator = SimulatorBackend::new(config).expect("simulator should construct");

        let mut acquired = 0_usize;
        let result = run_fixed_blocks(&device_config, 8, &mut || {
            if acquired >= 3 {
                return Err(SimulatorReadError("synthetic mid-run failure".to_string()));
            }
            acquired += 1;
            simulator
                .next_block()
                .map_err(|e| SimulatorReadError(e.to_string()))
        });

        let AcquisitionRunError::BackendRead {
            summary, blocks, ..
        } = result.expect_err("mid-run failure should surface as BackendRead")
        else {
            panic!("expected BackendRead error");
        };
        assert_eq!(
            blocks.len(),
            3,
            "blocks before the failure must be retained"
        );

        persist_partial_simulator_recording(&output_dir, &blocks, &summary)
            .expect("partial recording should be finalized");

        let expected_bytes = (3 * DEFAULT_CHANNEL_COUNT * DEFAULT_SAMPLES_PER_PACKET) as u64 * 2;
        assert_eq!(
            fs::metadata(output_dir.join("recording.kvraw"))
                .expect("partial raw recording should exist")
                .len(),
            expected_bytes
        );
        assert!(output_dir.join("integrity.json").exists());
        let log = fs::read_to_string(output_dir.join("log.txt")).expect("log should exist");
        assert!(log.contains("partial recording finalized"));

        let _ = fs::remove_dir_all(&output_dir);
    }

    /// A failure before any block is acquired leaves nothing to finalize.
    #[test]
    fn partial_recording_noop_when_no_blocks_acquired() {
        let output_dir = unique_output_dir("empty");
        let device_config = SimulatorConfig::default().device.clone();
        let result = run_fixed_blocks(&device_config, 1, &mut || {
            Err(SimulatorReadError("fails before first block".to_string()))
        });
        let AcquisitionRunError::BackendRead {
            summary, blocks, ..
        } = result.expect_err("immediate failure should surface as BackendRead")
        else {
            panic!("expected BackendRead error");
        };
        assert!(blocks.is_empty());

        persist_partial_simulator_recording(&output_dir, &blocks, &summary)
            .expect("no-op persistence should succeed");

        assert!(!output_dir.exists(), "no directory should be created");
    }
}
