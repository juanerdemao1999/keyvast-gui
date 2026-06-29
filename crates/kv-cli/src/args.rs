//! Command-line argument parsing for the Keyvast developer CLI.

#![allow(clippy::wildcard_imports)]

use crate::*;

pub fn parse_args<I, S>(args: I) -> Result<CliCommand, CliError>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut args = args.into_iter().map(Into::into);
    let Some(command) = args.next() else {
        return Err(CliError::MissingCommand);
    };

    match command.as_str() {
        "simulator-record" => parse_simulator_record_args(args),
        "simulator-pipeline" => parse_simulator_pipeline_args(args),
        "simulator-stream" => parse_simulator_stream_args(args),
        "benchmark" => parse_benchmark_args(args),
        "rhd-smoke" => parse_rhd_smoke_args(args),
        _ => Err(CliError::UnknownCommand { command }),
    }
}

pub(crate) fn parse_simulator_record_args(
    args: impl Iterator<Item = String>,
) -> Result<CliCommand, CliError> {
    let mut blocks = DEFAULT_BLOCKS;
    let mut output_dir: Option<PathBuf> = None;
    let mut drop_packet_ids = Vec::new();
    let mut args = args.peekable();

    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--blocks" => {
                let value = next_value(&mut args, "--blocks")?;
                blocks = parse_usize("--blocks", &value)?;
            }
            "--output" => {
                let value = next_value(&mut args, "--output")?;
                output_dir = Some(PathBuf::from(value));
            }
            "--drop-packet" => {
                let value = next_value(&mut args, "--drop-packet")?;
                drop_packet_ids.push(parse_u64("--drop-packet", &value)?);
            }
            _ => return Err(CliError::UnknownArgument { argument }),
        }
    }

    let output_dir = match output_dir {
        Some(output_dir) => output_dir,
        None => default_recording_output_dir()?,
    };

    Ok(CliCommand::SimulatorRecord(SimulatorRecordingOptions {
        output_dir,
        blocks,
        drop_packet_ids,
    }))
}

/// Default number of acquisition blocks when `--blocks` is omitted.
///
/// A value of 1 captures only a single block (~milliseconds of signal), which
/// silently produces a near-empty recording when callers forget the flag. This
/// default acquires a meaningful, non-trivial amount of data instead.
const DEFAULT_BLOCKS: usize = 1000;
const DEFAULT_RECORDER_CAPACITY: usize = 2048;
const DEFAULT_PREVIEW_CAPACITY: usize = 32;

pub(crate) fn parse_simulator_pipeline_args(
    args: impl Iterator<Item = String>,
) -> Result<CliCommand, CliError> {
    let mut blocks = DEFAULT_BLOCKS;
    let mut output_dir: Option<PathBuf> = None;
    let mut drop_packet_ids = Vec::new();
    let mut recorder_capacity = DEFAULT_RECORDER_CAPACITY;
    let mut preview_capacity = DEFAULT_PREVIEW_CAPACITY;
    let mut args = args.peekable();

    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--blocks" => {
                let value = next_value(&mut args, "--blocks")?;
                blocks = parse_usize("--blocks", &value)?;
            }
            "--output" => {
                let value = next_value(&mut args, "--output")?;
                output_dir = Some(PathBuf::from(value));
            }
            "--drop-packet" => {
                let value = next_value(&mut args, "--drop-packet")?;
                drop_packet_ids.push(parse_u64("--drop-packet", &value)?);
            }
            "--recorder-capacity" => {
                let value = next_value(&mut args, "--recorder-capacity")?;
                recorder_capacity = parse_usize("--recorder-capacity", &value)?;
            }
            "--preview-capacity" => {
                let value = next_value(&mut args, "--preview-capacity")?;
                preview_capacity = parse_usize("--preview-capacity", &value)?;
            }
            _ => return Err(CliError::UnknownArgument { argument }),
        }
    }

    let output_dir = match output_dir {
        Some(output_dir) => output_dir,
        None => default_recording_output_dir()?,
    };

    Ok(CliCommand::SimulatorPipeline(SimulatorPipelineOptions {
        output_dir,
        blocks,
        drop_packet_ids,
        recorder_capacity_blocks: recorder_capacity,
        preview_capacity_blocks: preview_capacity,
    }))
}

pub(crate) fn parse_simulator_stream_args(
    args: impl Iterator<Item = String>,
) -> Result<CliCommand, CliError> {
    let mut blocks = DEFAULT_BLOCKS;
    let mut output_dir: Option<PathBuf> = None;
    let mut drop_packet_ids = Vec::new();
    let mut recorder_capacity = DEFAULT_RECORDER_CAPACITY;
    let mut preview_capacity = DEFAULT_PREVIEW_CAPACITY;
    let mut args = args.peekable();

    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--blocks" => {
                let value = next_value(&mut args, "--blocks")?;
                blocks = parse_usize("--blocks", &value)?;
            }
            "--output" => {
                let value = next_value(&mut args, "--output")?;
                output_dir = Some(PathBuf::from(value));
            }
            "--drop-packet" => {
                let value = next_value(&mut args, "--drop-packet")?;
                drop_packet_ids.push(parse_u64("--drop-packet", &value)?);
            }
            "--recorder-capacity" => {
                let value = next_value(&mut args, "--recorder-capacity")?;
                recorder_capacity = parse_usize("--recorder-capacity", &value)?;
            }
            "--preview-capacity" => {
                let value = next_value(&mut args, "--preview-capacity")?;
                preview_capacity = parse_usize("--preview-capacity", &value)?;
            }
            _ => return Err(CliError::UnknownArgument { argument }),
        }
    }

    let output_dir = match output_dir {
        Some(output_dir) => output_dir,
        None => default_recording_output_dir()?,
    };

    Ok(CliCommand::SimulatorStream(SimulatorPipelineOptions {
        output_dir,
        blocks,
        drop_packet_ids,
        recorder_capacity_blocks: recorder_capacity,
        preview_capacity_blocks: preview_capacity,
    }))
}

pub(crate) fn parse_benchmark_args(
    args: impl Iterator<Item = String>,
) -> Result<CliCommand, CliError> {
    let mut output_dir: Option<PathBuf> = None;
    let mut duration: Option<f64> = None;
    let mut channel_count: Option<usize> = None;
    let mut sample_rate: Option<f64> = None;
    let mut samples_per_packet: Option<usize> = None;
    let mut preset: Option<BenchmarkPreset> = None;
    let mut drop_packet_ids = Vec::new();
    let mut recorder_capacity = DEFAULT_RECORDER_CAPACITY;
    let mut preview_capacity = DEFAULT_PREVIEW_CAPACITY;
    let mut args = args.peekable();

    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--preset" => {
                let value = next_value(&mut args, "--preset")?;
                preset = Some(parse_benchmark_preset(&value)?);
            }
            "--duration" => {
                let value = next_value(&mut args, "--duration")?;
                duration = Some(parse_f64("--duration", &value)?);
            }
            "--channels" => {
                let value = next_value(&mut args, "--channels")?;
                channel_count = Some(parse_usize("--channels", &value)?);
            }
            "--sample-rate" => {
                let value = next_value(&mut args, "--sample-rate")?;
                sample_rate = Some(parse_f64("--sample-rate", &value)?);
            }
            "--samples-per-packet" => {
                let value = next_value(&mut args, "--samples-per-packet")?;
                samples_per_packet = Some(parse_usize("--samples-per-packet", &value)?);
            }
            "--output" => {
                let value = next_value(&mut args, "--output")?;
                output_dir = Some(PathBuf::from(value));
            }
            "--drop-packet" => {
                let value = next_value(&mut args, "--drop-packet")?;
                drop_packet_ids.push(parse_u64("--drop-packet", &value)?);
            }
            "--recorder-capacity" => {
                let value = next_value(&mut args, "--recorder-capacity")?;
                recorder_capacity = parse_usize("--recorder-capacity", &value)?;
            }
            "--preview-capacity" => {
                let value = next_value(&mut args, "--preview-capacity")?;
                preview_capacity = parse_usize("--preview-capacity", &value)?;
            }
            _ => return Err(CliError::UnknownArgument { argument }),
        }
    }

    let duration_seconds = match (&preset, duration) {
        // An explicit --duration always wins, even alongside a preset, so it is
        // never silently dropped.
        (_, Some(d)) => d,
        (Some(p), None) => p.duration_seconds(),
        (None, None) => 10.0,
    };

    let channel_count = match (&preset, channel_count) {
        (_, Some(c)) => c,
        (Some(p), None) => p.channel_count().unwrap_or(DEFAULT_CHANNEL_COUNT),
        (None, None) => DEFAULT_CHANNEL_COUNT,
    };

    if channel_count == 0 {
        return Err(CliError::NonPositiveValue { flag: "--channels" });
    }
    if sample_rate.is_some_and(|rate| !(rate.is_finite() && rate > 0.0)) {
        return Err(CliError::NonPositiveValue {
            flag: "--sample-rate",
        });
    }

    let output_dir = match output_dir {
        Some(output_dir) => output_dir,
        None => default_recording_output_dir()?,
    };

    Ok(CliCommand::Benchmark(BenchmarkOptions {
        output_dir,
        duration_seconds,
        channel_count,
        sample_rate: sample_rate.unwrap_or(DEFAULT_SAMPLE_RATE),
        samples_per_packet: samples_per_packet.unwrap_or(DEFAULT_SAMPLES_PER_PACKET),
        recorder_capacity_blocks: recorder_capacity,
        preview_capacity_blocks: preview_capacity,
        drop_packet_ids,
    }))
}

pub(crate) fn parse_rhd_smoke_args(
    args: impl Iterator<Item = String>,
) -> Result<CliCommand, CliError> {
    let mut blocks = DEFAULT_BLOCKS;
    let mut enabled_streams = 2_usize;
    let mut sample_rate: Option<f64> = None;
    let mut raw_input: Option<PathBuf> = None;
    let mut bitfile_path = default_rhd_bitfile_path();
    let mut frontpanel_dll_path: Option<PathBuf> = None;
    let mut serial: Option<String> = None;
    let mut output_dir: Option<PathBuf> = None;
    let mut cable_length_meters = DEFAULT_CABLE_LENGTH_METERS;
    let mut args = args.peekable();

    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--blocks" => {
                let value = next_value(&mut args, "--blocks")?;
                blocks = parse_usize("--blocks", &value)?;
            }
            "--streams" | "--enabled-streams" => {
                let value = next_value(&mut args, "--streams")?;
                enabled_streams = parse_usize("--streams", &value)?;
            }
            "--cable-length" => {
                let value = next_value(&mut args, "--cable-length")?;
                cable_length_meters = parse_f64("--cable-length", &value)?;
            }
            "--sample-rate" => {
                let value = next_value(&mut args, "--sample-rate")?;
                sample_rate = Some(parse_f64("--sample-rate", &value)?);
            }
            "--raw-input" => {
                let value = next_value(&mut args, "--raw-input")?;
                raw_input = Some(PathBuf::from(value));
            }
            "--bitfile" => {
                let value = next_value(&mut args, "--bitfile")?;
                bitfile_path = PathBuf::from(value);
            }
            "--frontpanel-dll" => {
                let value = next_value(&mut args, "--frontpanel-dll")?;
                frontpanel_dll_path = Some(PathBuf::from(value));
            }
            "--serial" => {
                serial = Some(next_value(&mut args, "--serial")?);
            }
            "--output" => {
                let value = next_value(&mut args, "--output")?;
                output_dir = Some(PathBuf::from(value));
            }
            _ => return Err(CliError::UnknownArgument { argument }),
        }
    }

    if sample_rate.is_some_and(|rate| !(rate.is_finite() && rate > 0.0)) {
        return Err(CliError::NonPositiveValue {
            flag: "--sample-rate",
        });
    }

    let output_dir = match output_dir {
        Some(output_dir) => output_dir,
        None => default_recording_output_dir()?,
    };

    Ok(CliCommand::RhdSmoke(RhdSmokeOptions {
        output_dir,
        blocks,
        enabled_streams,
        sample_rate: sample_rate.unwrap_or(DEFAULT_RHD_SAMPLE_RATE),
        raw_input,
        bitfile_path,
        frontpanel_dll_path,
        serial,
        cable_length_meters,
    }))
}

pub(crate) fn default_rhd_bitfile_path() -> PathBuf {
    let name = kv_rhd::DEFAULT_RHD_BITFILE_NAME;
    // Resolve at run time, not compile time: the FPGA bitfile ships next to the
    // installed binary, so look beside the running executable first and fall
    // back to the current working directory. Baking in CARGO_MANIFEST_DIR would
    // point at the build machine's source tree, which never exists on the
    // deployment host.
    if let Ok(exe) = std::env::current_exe() {
        let candidate = exe.parent().map(|dir| dir.join(name));
        if let Some(candidate) = candidate.filter(|path| path.exists()) {
            return candidate;
        }
    }
    PathBuf::from(name)
}

pub(crate) fn parse_benchmark_preset(name: &str) -> Result<BenchmarkPreset, CliError> {
    match name {
        "smoke" => Ok(BenchmarkPreset::Smoke),
        "recorder" => Ok(BenchmarkPreset::Recorder),
        "stress-128" => Ok(BenchmarkPreset::Stress128),
        "stress-256" => Ok(BenchmarkPreset::Stress256),
        "endurance" => Ok(BenchmarkPreset::Endurance),
        _ => Err(CliError::UnknownPreset {
            name: name.to_string(),
        }),
    }
}

pub(crate) fn default_recording_output_dir() -> Result<PathBuf, CliError> {
    Ok(PathBuf::from(run_directory_name_utc(SystemTime::now())?))
}

pub fn run_directory_name_utc(timestamp: SystemTime) -> Result<String, CliError> {
    let duration = timestamp
        .duration_since(UNIX_EPOCH)
        .map_err(|_| CliError::SystemTimeBeforeUnixEpoch)?;
    let total_seconds = duration.as_secs();
    let days = total_seconds / 86_400;
    let seconds_in_day = total_seconds % 86_400;
    let (year, month, day) = civil_date_from_unix_days(days as i64);
    let hour = seconds_in_day / 3_600;
    let minute = (seconds_in_day % 3_600) / 60;
    let second = seconds_in_day % 60;

    Ok(format!(
        "run-{year:04}{month:02}{day:02}-{hour:02}{minute:02}{second:02}"
    ))
}

pub(crate) fn civil_date_from_unix_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    let year = year + if month <= 2 { 1 } else { 0 };

    (year as i32, month as u32, day as u32)
}

pub(crate) fn next_value(
    args: &mut impl Iterator<Item = String>,
    flag: &'static str,
) -> Result<String, CliError> {
    args.next().ok_or(CliError::MissingArgumentValue { flag })
}

pub(crate) fn parse_usize(flag: &'static str, value: &str) -> Result<usize, CliError> {
    value.parse().map_err(|_| CliError::InvalidNumber {
        flag,
        value: value.to_string(),
    })
}

pub(crate) fn parse_f64(flag: &'static str, value: &str) -> Result<f64, CliError> {
    value.parse().map_err(|_| CliError::InvalidNumber {
        flag,
        value: value.to_string(),
    })
}

pub(crate) fn parse_u64(flag: &'static str, value: &str) -> Result<u64, CliError> {
    value.parse().map_err(|_| CliError::InvalidNumber {
        flag,
        value: value.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn benchmark(args: &[&str]) -> Result<BenchmarkOptions, CliError> {
        let mut full = vec!["benchmark"];
        full.extend_from_slice(args);
        match parse_args(full) {
            Ok(CliCommand::Benchmark(options)) => Ok(options),
            Ok(other) => panic!("expected a benchmark command, got {other:?}"),
            Err(error) => Err(error),
        }
    }

    #[test]
    fn explicit_duration_overrides_a_preset() {
        // H11: --duration must not be silently dropped when a preset is present.
        let options = benchmark(&["--preset", "smoke", "--duration", "3.5"]).expect("parses");
        assert!((options.duration_seconds - 3.5).abs() < f64::EPSILON);
    }

    #[test]
    fn zero_channels_is_rejected() {
        // I3: --channels 0 must error rather than produce a degenerate run.
        let error = benchmark(&["--channels", "0"]).expect_err("zero channels is invalid");
        assert!(matches!(
            error,
            CliError::NonPositiveValue { flag: "--channels" }
        ));
    }

    #[test]
    fn non_positive_sample_rate_is_rejected() {
        // I3: --sample-rate 0 must error rather than produce a degenerate run.
        let error = benchmark(&["--sample-rate", "0"]).expect_err("zero sample rate is invalid");
        assert!(matches!(
            error,
            CliError::NonPositiveValue {
                flag: "--sample-rate"
            }
        ));
    }

    #[test]
    fn civil_date_matches_known_unix_days() {
        // L31: cover the proleptic-Gregorian conversion at known epochs.
        assert_eq!(civil_date_from_unix_days(0), (1970, 1, 1));
        assert_eq!(civil_date_from_unix_days(31), (1970, 2, 1));
        // 2000-01-01 is 10_957 days after the unix epoch.
        assert_eq!(civil_date_from_unix_days(10_957), (2000, 1, 1));
        // A pre-epoch day stays in 1969.
        assert_eq!(civil_date_from_unix_days(-1), (1969, 12, 31));
    }

    #[test]
    fn civil_date_handles_leap_year_month_boundaries() {
        // L31: the proleptic-Gregorian rule keeps Feb 29 in years divisible by
        // 400 (2000) but drops it in century years that are not (2100).
        assert_eq!(civil_date_from_unix_days(11_016), (2000, 2, 29));
        assert_eq!(civil_date_from_unix_days(11_017), (2000, 3, 1));
        // 2100 is not a leap year, so Feb has 28 days and rolls straight to Mar.
        assert_eq!(civil_date_from_unix_days(47_540), (2100, 2, 28));
        assert_eq!(civil_date_from_unix_days(47_541), (2100, 3, 1));
        // A common year still ends on Dec 31.
        assert_eq!(civil_date_from_unix_days(20_088), (2024, 12, 31));
    }
}
