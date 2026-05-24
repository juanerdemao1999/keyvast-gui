use std::{env, process::ExitCode};

use kv_cli::{parse_args, run_command};

fn main() -> ExitCode {
    match parse_args(env::args().skip(1)).and_then(run_command) {
        Ok(result) => {
            println!("output_dir={}", result.recording.output_dir.display());
            println!("acquired_blocks={}", result.acquisition.acquired_blocks);
            println!("written_samples={}", result.recording.written_samples);
            println!(
                "missing_packets={}",
                result.integrity.summary.missing_packets
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}
