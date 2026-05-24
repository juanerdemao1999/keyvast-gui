use std::{env, process::ExitCode};

use kv_cli::{CommandResult, parse_args, run_command};

fn main() -> ExitCode {
    match parse_args(env::args().skip(1)).and_then(run_command) {
        Ok(CommandResult::Record(result)) => {
            println!("output_dir={}", result.recording.output_dir.display());
            println!("acquired_blocks={}", result.acquisition.acquired_blocks);
            println!("written_samples={}", result.recording.written_samples);
            println!(
                "missing_packets={}",
                result.integrity.summary.missing_packets
            );
            ExitCode::SUCCESS
        }
        Ok(CommandResult::Pipeline(result)) => {
            println!("output_dir={}", result.recording.output_dir.display());
            println!("written_samples={}", result.recording.written_samples);
            println!(
                "missing_packets={}",
                result.integrity.summary.missing_packets
            );
            println!("wall_clock_seconds={:.6}", result.timing.wall_clock_seconds);
            println!("recorder_dropped_blocks={}", result.recorder_dropped_blocks);
            println!("preview_dropped_blocks={}", result.preview_dropped_blocks);
            println!("measurement_kind=measured");
            ExitCode::SUCCESS
        }
        Ok(CommandResult::Stream(result)) => {
            println!("output_dir={}", result.recording.output_dir.display());
            println!("written_samples={}", result.recording.written_samples);
            println!(
                "missing_packets={}",
                result.integrity.summary.missing_packets
            );
            println!("wall_clock_seconds={:.6}", result.timing.wall_clock_seconds);
            println!("recorder_dropped_blocks={}", result.recorder_dropped_blocks);
            println!("preview_dropped_blocks={}", result.preview_dropped_blocks);
            if let Some(latency) = result.max_write_latency_us {
                println!("max_write_latency_us={latency}");
            }
            println!("measurement_kind=measured_streaming");
            ExitCode::SUCCESS
        }
        Ok(CommandResult::Benchmark(result)) => {
            println!("output_dir={}", result.recording.output_dir.display());
            println!(
                "requested_duration_seconds={:.1}",
                result.requested_duration_seconds
            );
            println!("computed_block_count={}", result.computed_block_count);
            println!("written_samples={}", result.recording.written_samples);
            println!(
                "missing_packets={}",
                result.integrity.summary.missing_packets
            );
            println!("wall_clock_seconds={:.6}", result.timing.wall_clock_seconds);
            println!("recorder_dropped_blocks={}", result.recorder_dropped_blocks);
            println!("preview_dropped_blocks={}", result.preview_dropped_blocks);
            if let Some(latency) = result.max_write_latency_us {
                println!("max_write_latency_us={latency}");
            }
            println!("measurement_kind=measured_streaming");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}
