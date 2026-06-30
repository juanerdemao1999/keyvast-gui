//! Human-readable benchmark summaries, log lines and acquisition events
//! derived from integrity reports.

#![allow(clippy::wildcard_imports)]

use crate::*;

pub(crate) fn streaming_benchmark_summary(
    result: &StreamingPipelineResult,
    device: &kv_types::DeviceConfig,
    process_metrics: Option<&ProcessMetrics>,
    requested_duration_seconds: Option<f64>,
) -> BenchmarkSummary {
    BenchmarkSummary {
        measurement_kind: "measured_streaming".to_string(),
        duration_seconds: recorded_duration_seconds(
            result.integrity.summary.written_samples,
            device.channel_count,
            device.sample_rate,
        ),
        wall_clock_seconds: Some(result.timing.wall_clock_seconds),
        requested_duration_seconds,
        channel_count: device.channel_count,
        sample_rate: device.sample_rate,
        expected_samples: result.integrity.summary.expected_samples,
        written_samples: result.integrity.summary.written_samples,
        missing_packets: result.integrity.summary.missing_packets,
        crc_errors: result.integrity.summary.crc_errors,
        timestamp_discontinuities: result.integrity.summary.timestamp_discontinuities,
        byte_count: result.recording.byte_count,
        average_write_mb_s: average_write_mb_s(
            result.recording.byte_count,
            result.timing.wall_clock_seconds,
        ),
        max_write_latency_ms: result.max_write_latency_us.map(|us| us as f64 / 1_000.0),
        p50_write_latency_ms: result
            .latency_distribution
            .as_ref()
            .map(|d| d.p50_us as f64 / 1_000.0),
        p95_write_latency_ms: result
            .latency_distribution
            .as_ref()
            .map(|d| d.p95_us as f64 / 1_000.0),
        p99_write_latency_ms: result
            .latency_distribution
            .as_ref()
            .map(|d| d.p99_us as f64 / 1_000.0),
        max_buffer_occupancy: Some(
            result
                .recorder_status
                .occupancy
                .max(result.preview_status.occupancy),
        ),
        cpu_percent_avg: process_metrics.map(|m| m.cpu_percent_avg),
        memory_mb_max: process_metrics.map(|m| m.memory_mb_max),
    }
}

pub(crate) fn rhd_smoke_benchmark_summary(
    acquisition: &AcquisitionRunSummary,
    recording: &RecordingSummary,
    integrity: &IntegrityReport,
    hardware: bool,
) -> BenchmarkSummary {
    let duration_seconds = recorded_duration_seconds(
        integrity.summary.written_samples,
        acquisition.status.channel_count,
        acquisition.status.sample_rate,
    );

    BenchmarkSummary {
        measurement_kind: if hardware {
            "rhd_hardware_smoke".to_string()
        } else {
            "rhd_raw_input".to_string()
        },
        duration_seconds,
        wall_clock_seconds: None,
        requested_duration_seconds: None,
        channel_count: acquisition.status.channel_count,
        sample_rate: acquisition.status.sample_rate,
        expected_samples: integrity.summary.expected_samples,
        written_samples: integrity.summary.written_samples,
        missing_packets: integrity.summary.missing_packets,
        crc_errors: integrity.summary.crc_errors,
        timestamp_discontinuities: integrity.summary.timestamp_discontinuities,
        byte_count: recording.byte_count,
        average_write_mb_s: average_write_mb_s(recording.byte_count, duration_seconds),
        max_write_latency_ms: None,
        p50_write_latency_ms: None,
        p95_write_latency_ms: None,
        p99_write_latency_ms: None,
        max_buffer_occupancy: None,
        cpu_percent_avg: None,
        memory_mb_max: None,
    }
}

pub(crate) fn pipeline_benchmark_summary(
    pipeline: &PipelineResult,
    recording: &RecordingSummary,
) -> BenchmarkSummary {
    let first_block = pipeline.recorded_blocks.first();
    let channel_count = first_block.map_or(0, |b| b.channel_count);
    let sample_rate = first_block.map_or(0.0, |b| b.sample_rate);

    BenchmarkSummary {
        measurement_kind: "measured".to_string(),
        duration_seconds: recorded_duration_seconds(
            pipeline.integrity.summary.written_samples,
            channel_count,
            sample_rate,
        ),
        wall_clock_seconds: Some(pipeline.timing.wall_clock_seconds),
        requested_duration_seconds: None,
        channel_count,
        sample_rate,
        expected_samples: pipeline.integrity.summary.expected_samples,
        written_samples: pipeline.integrity.summary.written_samples,
        missing_packets: pipeline.integrity.summary.missing_packets,
        crc_errors: pipeline.integrity.summary.crc_errors,
        timestamp_discontinuities: pipeline.integrity.summary.timestamp_discontinuities,
        byte_count: recording.byte_count,
        average_write_mb_s: average_write_mb_s(
            recording.byte_count,
            pipeline.timing.wall_clock_seconds,
        ),
        max_write_latency_ms: None,
        p50_write_latency_ms: None,
        p95_write_latency_ms: None,
        p99_write_latency_ms: None,
        max_buffer_occupancy: Some(
            pipeline
                .recorder_status
                .occupancy
                .max(pipeline.preview_status.occupancy),
        ),
        cpu_percent_avg: None,
        memory_mb_max: None,
    }
}

pub(crate) fn simulator_recording_log_lines(integrity: &IntegrityReport) -> Vec<String> {
    let mut lines = vec![
        "[INFO] acquisition started".to_string(),
        format!(
            "[INFO] acquired_blocks={}",
            integrity.summary.observed_packets
        ),
    ];

    for gap in &integrity.packet_gaps {
        lines.push(format!(
            "[WARN] missing packet expected={} observed={} missing={}",
            gap.expected_packet_id, gap.observed_packet_id, gap.missing_count
        ));
    }

    for discontinuity in &integrity.timestamp_discontinuities {
        lines.push(format!(
            "[WARN] timestamp discontinuity packet={} expected={} observed={}",
            discontinuity.packet_id,
            discontinuity.expected_timestamp_start,
            discontinuity.observed_timestamp_start
        ));
    }

    if integrity.summary.buffer_overflows > 0 {
        lines.push(format!(
            "[WARN] buffer overflow dropped_blocks={}",
            integrity.summary.buffer_overflows
        ));
    }

    lines.push("[INFO] recorder flushed".to_string());
    lines.push("[INFO] acquisition stopped cleanly".to_string());
    lines
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub(crate) fn simulator_recording_events(
    integrity: &IntegrityReport,
    ttl_events: Vec<AcquisitionEvent>,
) -> Vec<AcquisitionEvent> {
    let mut events = vec![AcquisitionEvent::Started {
        timestamp_host_ms: now_ms(),
    }];

    events.extend(ttl_events);

    for gap in &integrity.packet_gaps {
        events.push(AcquisitionEvent::PacketMissing {
            expected_packet_id: gap.expected_packet_id,
            observed_packet_id: gap.observed_packet_id,
            missing_count: gap.missing_count,
        });
    }

    push_buffer_overflow_event(&mut events, integrity);

    events.push(AcquisitionEvent::Stopped {
        timestamp_host_ms: now_ms(),
    });
    events
}

/// Emit a `BufferOverflow` event when the pipeline dropped any block, so the
/// `events.csv` audit trail records overflow instead of swallowing it.
/// `buffer_occupancy` is 1.0 because a drop only happens when a consumer queue
/// is already full.
fn push_buffer_overflow_event(events: &mut Vec<AcquisitionEvent>, integrity: &IntegrityReport) {
    if integrity.summary.buffer_overflows > 0 {
        events.push(AcquisitionEvent::BufferOverflow {
            dropped_blocks: integrity.summary.buffer_overflows,
            buffer_occupancy: 1.0,
        });
    }
}

pub(crate) fn rhd_smoke_log_lines(integrity: &IntegrityReport, hardware: bool) -> Vec<String> {
    let mut lines = vec![
        if hardware {
            "[INFO] rhd hardware smoke started".to_string()
        } else {
            "[INFO] rhd raw-input smoke started".to_string()
        },
        format!(
            "[INFO] acquired_blocks={}",
            integrity.summary.observed_packets
        ),
    ];

    for gap in &integrity.packet_gaps {
        lines.push(format!(
            "[WARN] missing packet expected={} observed={} missing={}",
            gap.expected_packet_id, gap.observed_packet_id, gap.missing_count
        ));
    }

    for discontinuity in &integrity.timestamp_discontinuities {
        lines.push(format!(
            "[WARN] timestamp discontinuity packet={} expected={} observed={}",
            discontinuity.packet_id,
            discontinuity.expected_timestamp_start,
            discontinuity.observed_timestamp_start
        ));
    }

    if integrity.summary.buffer_overflows > 0 {
        lines.push(format!(
            "[WARN] buffer overflow dropped_blocks={}",
            integrity.summary.buffer_overflows
        ));
    }

    lines.push("[INFO] recorder flushed".to_string());
    lines.push("[INFO] rhd smoke stopped cleanly".to_string());
    lines
}

pub(crate) fn rhd_smoke_events(integrity: &IntegrityReport) -> Vec<AcquisitionEvent> {
    let mut events = vec![AcquisitionEvent::Started {
        timestamp_host_ms: now_ms(),
    }];

    for gap in &integrity.packet_gaps {
        events.push(AcquisitionEvent::PacketMissing {
            expected_packet_id: gap.expected_packet_id,
            observed_packet_id: gap.observed_packet_id,
            missing_count: gap.missing_count,
        });
    }

    push_buffer_overflow_event(&mut events, integrity);

    events.push(AcquisitionEvent::Stopped {
        timestamp_host_ms: now_ms(),
    });
    events
}

pub(crate) fn simulator_benchmark_summary(
    acquisition: &AcquisitionRunSummary,
    recording: &RecordingSummary,
    integrity: &IntegrityReport,
) -> BenchmarkSummary {
    let duration_seconds = recorded_duration_seconds(
        integrity.summary.written_samples,
        acquisition.status.channel_count,
        acquisition.status.sample_rate,
    );

    BenchmarkSummary {
        measurement_kind: "simulator_estimate".to_string(),
        duration_seconds,
        wall_clock_seconds: None,
        requested_duration_seconds: None,
        channel_count: acquisition.status.channel_count,
        sample_rate: acquisition.status.sample_rate,
        expected_samples: integrity.summary.expected_samples,
        written_samples: integrity.summary.written_samples,
        missing_packets: integrity.summary.missing_packets,
        crc_errors: integrity.summary.crc_errors,
        timestamp_discontinuities: integrity.summary.timestamp_discontinuities,
        byte_count: recording.byte_count,
        average_write_mb_s: average_write_mb_s(recording.byte_count, duration_seconds),
        max_write_latency_ms: None,
        p50_write_latency_ms: None,
        p95_write_latency_ms: None,
        p99_write_latency_ms: None,
        max_buffer_occupancy: None,
        cpu_percent_avg: None,
        memory_mb_max: None,
    }
}

pub(crate) fn recorded_duration_seconds(
    written_samples: u64,
    channel_count: usize,
    sample_rate: f64,
) -> f64 {
    if channel_count == 0 || !sample_rate.is_finite() || sample_rate <= 0.0 {
        return 0.0;
    }

    written_samples as f64 / channel_count as f64 / sample_rate
}

pub(crate) fn average_write_mb_s(byte_count: u64, duration_seconds: f64) -> f64 {
    if !duration_seconds.is_finite() || duration_seconds <= 0.0 {
        return 0.0;
    }

    byte_count as f64 / duration_seconds / 1_000_000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use kv_types::IntegritySummary;

    fn report_with_overflows(dropped: u64) -> IntegrityReport {
        IntegrityReport {
            summary: IntegritySummary {
                buffer_overflows: dropped,
                ..IntegritySummary::default()
            },
            packet_gaps: Vec::new(),
            timestamp_discontinuities: Vec::new(),
        }
    }

    #[test]
    fn simulator_events_emit_buffer_overflow_when_blocks_dropped() {
        let events = simulator_recording_events(&report_with_overflows(7), Vec::new());
        let overflow = events.iter().find_map(|e| match e {
            AcquisitionEvent::BufferOverflow {
                dropped_blocks,
                buffer_occupancy,
            } => Some((*dropped_blocks, *buffer_occupancy)),
            _ => None,
        });
        assert_eq!(overflow, Some((7, 1.0)));
    }

    #[test]
    fn rhd_events_emit_buffer_overflow_when_blocks_dropped() {
        let events = rhd_smoke_events(&report_with_overflows(3));
        assert!(events.iter().any(|e| matches!(
            e,
            AcquisitionEvent::BufferOverflow {
                dropped_blocks: 3,
                ..
            }
        )));
    }

    #[test]
    fn no_buffer_overflow_event_without_drops() {
        let events = simulator_recording_events(&report_with_overflows(0), Vec::new());
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, AcquisitionEvent::BufferOverflow { .. }))
        );
    }

    #[test]
    fn log_lines_report_buffer_overflow() {
        let lines = simulator_recording_log_lines(&report_with_overflows(5));
        assert!(
            lines
                .iter()
                .any(|l| l.contains("buffer overflow") && l.contains("dropped_blocks=5"))
        );
    }
}
