//! Simulator backend for hardware-independent acquisition tests.

use std::fmt;
use std::time::{Duration, Instant};

use kv_types::{DeviceBackendKind, DeviceConfig, SampleBlock, SampleBlockError};

pub const DEFAULT_SIMULATOR_SEED: u64 = 0x4b56_5354_0000_0001;

/// Synthetic LFP oscillation frequency. The triangle carrier period is derived
/// from the configured sample rate so the waveform stays a fixed real-world
/// frequency regardless of packet size (was coupled to `samples_per_packet`).
const SIM_LFP_FREQ_HZ: f64 = 8.0;

/// Spike-trial window length in Hz: one independent spike opportunity per
/// `sample_rate / SIM_SPIKE_TRIAL_HZ` samples. Decouples spike timing from the
/// packet boundary so spikes no longer burst once per whole packet.
const SIM_SPIKE_TRIAL_HZ: f64 = 250.0;

#[derive(Debug, Clone, PartialEq)]
pub struct SimulatorConfig {
    pub device: DeviceConfig,
    pub seed: u64,
    pub stream_id: u32,
    pub drop_packet_ids: Vec<u64>,
    /// When `true`, [`SimulatorBackend::next_block`] throttles block production
    /// to the device's real-time cadence so benchmarks observe wall-clock
    /// timing instead of running as fast as the CPU allows. Defaults to `false`.
    pub paced: bool,
}

impl Default for SimulatorConfig {
    fn default() -> Self {
        Self {
            device: DeviceConfig::simulator_default(),
            seed: DEFAULT_SIMULATOR_SEED,
            stream_id: 0,
            drop_packet_ids: Vec::new(),
            paced: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SimulatorBackend {
    config: SimulatorConfig,
    next_packet_id: u64,
    /// Wall-clock reference captured on the first paced block; used to compute
    /// each subsequent block's real-time deadline. `None` until paced output
    /// starts (or always `None` when pacing is disabled).
    paced_start: Option<Instant>,
}

impl SimulatorBackend {
    pub fn new(mut config: SimulatorConfig) -> Result<Self, SimulatorConfigError> {
        validate_device_config(&config.device)?;
        config.drop_packet_ids.sort_unstable();
        config.drop_packet_ids.dedup();

        Ok(Self {
            config,
            next_packet_id: 0,
            paced_start: None,
        })
    }

    pub fn config(&self) -> &SimulatorConfig {
        &self.config
    }

    pub fn next_block(&mut self) -> Result<SampleBlock, SimulatorError> {
        while self
            .config
            .drop_packet_ids
            .binary_search(&self.next_packet_id)
            .is_ok()
        {
            self.next_packet_id = match self.next_packet_id.checked_add(1) {
                Some(id) => id,
                None => break, // u64::MAX: stop skipping to avoid infinite loop
            };
        }

        let packet_id = self.next_packet_id;
        let timestamp_start =
            packet_id.saturating_mul(self.config.device.samples_per_packet as u64);
        let ttl_in_per_sample = self.ttl_in_per_sample(timestamp_start);
        // `ttl_bits` keeps the last sample's word for backward compatibility.
        let ttl_bits = ttl_in_per_sample
            .as_ref()
            .and_then(|words| words.last().copied())
            .unwrap_or(0);
        let block = SampleBlock {
            device_id: self.config.device.device_id.clone(),
            stream_id: self.config.stream_id,
            packet_id,
            timestamp_start,
            sample_rate: self.config.device.sample_rate,
            channel_count: self.config.device.channel_count,
            samples_per_channel: self.config.device.samples_per_packet,
            ttl_bits,
            data: self.samples_for_packet(packet_id, timestamp_start),
            aux_data: None,
            board_adc_data: None,
            ttl_in_per_sample,
            ttl_out_per_sample: None,
            // Synthetic source: keep generated blocks deterministic (no wall-clock).
            host_time_ns: None,
        };

        block
            .validate_against_ttl_lines(self.config.device.ttl_line_count)
            .map_err(SimulatorError::InvalidGeneratedBlock)?;
        self.next_packet_id = self.next_packet_id.saturating_add(1);

        if self.config.paced {
            self.pace_to_deadline(timestamp_start);
        }

        Ok(block)
    }

    /// Sleep until the wall-clock time at which `timestamp_start + one packet`
    /// worth of samples would have arrived from real hardware. The first paced
    /// block anchors the clock; later blocks sleep for whatever slack remains,
    /// so transient overruns are absorbed rather than accumulated.
    fn pace_to_deadline(&mut self, timestamp_start: u64) {
        let sample_rate = self.config.device.sample_rate;
        if !sample_rate.is_finite() || sample_rate <= 0.0 {
            return;
        }
        let anchor = *self.paced_start.get_or_insert_with(Instant::now);
        let samples_elapsed =
            timestamp_start.saturating_add(self.config.device.samples_per_packet as u64);
        let deadline = Duration::from_secs_f64(samples_elapsed as f64 / sample_rate);
        if let Some(remaining) = deadline.checked_sub(anchor.elapsed()) {
            std::thread::sleep(remaining);
        }
    }

    fn samples_for_packet(&self, packet_id: u64, timestamp_start: u64) -> Vec<i16> {
        let sample_count = self
            .config
            .device
            .channel_count
            .saturating_mul(self.config.device.samples_per_packet);
        let mut data = Vec::with_capacity(sample_count);

        for sample_offset in 0..self.config.device.samples_per_packet {
            let sample_index = timestamp_start.saturating_add(sample_offset as u64);
            for channel in 0..self.config.device.channel_count {
                data.push(self.sample_value(packet_id, sample_index, channel));
            }
        }

        data
    }

    fn sample_value(&self, packet_id: u64, sample_index: u64, channel: usize) -> i16 {
        let sample_rate = self.config.device.sample_rate;
        let noise_seed = self.config.seed
            ^ packet_id.rotate_left(13)
            ^ sample_index.rotate_left(7)
            ^ (channel as u64).rotate_left(29);
        let noise = (mix_u64(noise_seed) % 41) as i32 - 20;
        let lfp = triangle_wave(
            sample_index.saturating_add((channel as u64).saturating_mul(3)),
            sample_rate,
        );
        let spike = spike_component(self.config.seed, sample_index, channel, sample_rate);
        clamp_i16(noise + lfp + spike)
    }

    fn ttl_line_mask(&self) -> u32 {
        if !self.config.device.ttl_enabled || self.config.device.ttl_line_count == 0 {
            return 0;
        }
        if self.config.device.ttl_line_count == u32::BITS as usize {
            u32::MAX
        } else {
            (1_u32 << self.config.device.ttl_line_count) - 1
        }
    }

    /// Per-sample TTL input words for a packet, or `None` when TTL is disabled.
    /// Each sample gets an independent deterministic word so downstream code that
    /// consumes `ttl_in_per_sample` has realistic per-sample edges to test against.
    fn ttl_in_per_sample(&self, timestamp_start: u64) -> Option<Vec<u32>> {
        let mask = self.ttl_line_mask();
        if mask == 0 {
            return None;
        }
        let spp = self.config.device.samples_per_packet;
        let mut words = Vec::with_capacity(spp);
        for offset in 0..spp {
            let sample_index = timestamp_start.saturating_add(offset as u64);
            words.push((mix_u64(self.config.seed ^ sample_index.rotate_left(11)) as u32) & mask);
        }
        Some(words)
    }
}

impl Default for SimulatorBackend {
    fn default() -> Self {
        // The default config is valid by construction, so build the backend
        // directly instead of unwrapping a Result with `.expect()`.
        let mut config = SimulatorConfig::default();
        config.drop_packet_ids.sort_unstable();
        config.drop_packet_ids.dedup();
        Self {
            config,
            next_packet_id: 0,
            paced_start: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SimulatorConfigError {
    NonSimulatorBackend {
        backend: DeviceBackendKind,
    },
    InvalidSampleRate,
    EmptyChannelSet,
    EmptyPacket,
    TtlLineCountOutOfRange {
        ttl_line_count: usize,
    },
    EnabledChannelOutOfRange {
        channel: usize,
        channel_count: usize,
    },
}

impl fmt::Display for SimulatorConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonSimulatorBackend { backend } => {
                write!(
                    formatter,
                    "simulator requires Simulator backend, got {backend:?}"
                )
            }
            Self::InvalidSampleRate => write!(formatter, "sample rate must be finite and positive"),
            Self::EmptyChannelSet => write!(formatter, "simulator requires at least one channel"),
            Self::EmptyPacket => write!(
                formatter,
                "simulator requires at least one sample per channel per packet"
            ),
            Self::TtlLineCountOutOfRange { ttl_line_count } => write!(
                formatter,
                "ttl line count {ttl_line_count} exceeds u32 ttl storage width"
            ),
            Self::EnabledChannelOutOfRange {
                channel,
                channel_count,
            } => write!(
                formatter,
                "enabled channel {channel} is outside configured channel count {channel_count}"
            ),
        }
    }
}

impl std::error::Error for SimulatorConfigError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SimulatorError {
    InvalidGeneratedBlock(SampleBlockError),
}

impl fmt::Display for SimulatorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidGeneratedBlock(error) => {
                write!(formatter, "generated simulator block is invalid: {error}")
            }
        }
    }
}

impl std::error::Error for SimulatorError {}

fn validate_device_config(config: &DeviceConfig) -> Result<(), SimulatorConfigError> {
    if config.backend != DeviceBackendKind::Simulator {
        return Err(SimulatorConfigError::NonSimulatorBackend {
            backend: config.backend,
        });
    }

    if !config.sample_rate.is_finite() || config.sample_rate <= 0.0 {
        return Err(SimulatorConfigError::InvalidSampleRate);
    }

    if config.channel_count == 0 {
        return Err(SimulatorConfigError::EmptyChannelSet);
    }

    if config.samples_per_packet == 0 {
        return Err(SimulatorConfigError::EmptyPacket);
    }

    if config.ttl_line_count > u32::BITS as usize {
        return Err(SimulatorConfigError::TtlLineCountOutOfRange {
            ttl_line_count: config.ttl_line_count,
        });
    }

    for &channel in &config.enabled_channels {
        if channel >= config.channel_count {
            return Err(SimulatorConfigError::EnabledChannelOutOfRange {
                channel,
                channel_count: config.channel_count,
            });
        }
    }

    Ok(())
}

fn mix_u64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

fn triangle_wave(sample_index: u64, sample_rate: f64) -> i32 {
    let period = sample_period(sample_rate, SIM_LFP_FREQ_HZ);
    let phase = (sample_index % period) as i32;
    let half_period = (period / 2) as i32;

    if phase < half_period {
        phase - (half_period / 2)
    } else {
        (period as i32 - phase) - (half_period / 2)
    }
}

fn spike_component(seed: u64, sample_index: u64, channel: usize, sample_rate: f64) -> i32 {
    // Channel-dependent spike rate: lower channels spike more often.
    // Modulus controls rarity — higher modulus = fewer spikes.
    let rarity = 512 + (channel as u64 % 8) * 128;

    // One independent spike trial per fixed-length window (in samples), so spike
    // timing no longer bursts on packet boundaries. The 3-sample biphasic
    // template is emitted only at the start of a trial that fires.
    let trial_len = sample_period(sample_rate, SIM_SPIKE_TRIAL_HZ).max(6);
    let trial = sample_index / trial_len;
    let phase = sample_index % trial_len;
    if phase > 2 {
        return 0;
    }

    let event_seed = seed ^ trial ^ (channel as u64 * 17);
    if mix_u64(event_seed).is_multiple_of(rarity) {
        match phase {
            0 => -180,
            1 => 260,
            2 => -80,
            _ => 0,
        }
    } else {
        0
    }
}

/// Number of samples in one period of `freq_hz` at `sample_rate`, clamped to a
/// sane minimum so a degenerate rate never yields a zero-length period.
fn sample_period(sample_rate: f64, freq_hz: f64) -> u64 {
    if !sample_rate.is_finite() || sample_rate <= 0.0 || freq_hz <= 0.0 {
        return 2;
    }
    ((sample_rate / freq_hz).round() as u64).max(2)
}

fn clamp_i16(value: i32) -> i16 {
    value.clamp(i16::MIN as i32, i16::MAX as i32) as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timing_device() -> DeviceConfig {
        let mut device = DeviceConfig::simulator_default();
        // 80 samples at 8 kHz => 10 ms of real time per block.
        device.sample_rate = 8_000.0;
        device.samples_per_packet = 80;
        device
    }

    #[test]
    fn paced_backend_throttles_to_real_time() {
        let device = timing_device();
        let blocks = 3u64;
        let expected = Duration::from_secs_f64(
            (blocks * device.samples_per_packet as u64) as f64 / device.sample_rate,
        );

        let mut paced = SimulatorBackend::new(SimulatorConfig {
            device: device.clone(),
            paced: true,
            ..SimulatorConfig::default()
        })
        .expect("paced config is valid");
        let start = Instant::now();
        for _ in 0..blocks {
            paced.next_block().expect("paced block");
        }
        let paced_elapsed = start.elapsed();
        assert!(
            paced_elapsed >= expected.mul_f64(0.8),
            "paced run finished too early: {paced_elapsed:?} < {expected:?}"
        );
    }

    #[test]
    fn unpaced_backend_runs_faster_than_real_time() {
        let device = timing_device();
        let blocks = 3u64;
        let expected = Duration::from_secs_f64(
            (blocks * device.samples_per_packet as u64) as f64 / device.sample_rate,
        );

        let mut unpaced = SimulatorBackend::new(SimulatorConfig {
            device,
            paced: false,
            ..SimulatorConfig::default()
        })
        .expect("default config is valid");
        let start = Instant::now();
        for _ in 0..blocks {
            unpaced.next_block().expect("block");
        }
        assert!(
            start.elapsed() < expected.mul_f64(0.5),
            "unpaced run should not block on real-time cadence"
        );
    }

    fn backend_with_ttl(ttl_enabled: bool, ttl_line_count: usize) -> SimulatorBackend {
        let mut device = DeviceConfig::simulator_default();
        device.ttl_enabled = ttl_enabled;
        device.ttl_line_count = ttl_line_count;
        SimulatorBackend::new(SimulatorConfig {
            device,
            ..SimulatorConfig::default()
        })
        .expect("ttl config is valid")
    }

    #[test]
    fn ttl_line_mask_covers_boundary_line_counts() {
        // L15/L16: a zero line count or disabled TTL collapses to no mask, a
        // partial width fills the low bits, and a full 32-line bank saturates
        // without shifting past the u32 width.
        assert_eq!(backend_with_ttl(true, 0).ttl_line_mask(), 0);
        assert_eq!(backend_with_ttl(false, 8).ttl_line_mask(), 0);
        assert_eq!(backend_with_ttl(true, 1).ttl_line_mask(), 0b1);
        assert_eq!(backend_with_ttl(true, 8).ttl_line_mask(), 0xff);
        assert_eq!(
            backend_with_ttl(true, 31).ttl_line_mask(),
            (1_u32 << 31) - 1
        );
        assert_eq!(backend_with_ttl(true, 32).ttl_line_mask(), u32::MAX);
    }

    #[test]
    fn ttl_in_per_sample_is_none_when_disabled_and_masked_when_enabled() {
        // L15/L16: disabling TTL drops the per-sample words entirely, while an
        // enabled bank yields one word per sample, each confined to the mask.
        assert!(backend_with_ttl(false, 8).ttl_in_per_sample(0).is_none());
        assert!(backend_with_ttl(true, 0).ttl_in_per_sample(0).is_none());

        let backend = backend_with_ttl(true, 4);
        let spp = backend.config.device.samples_per_packet;
        let words = backend
            .ttl_in_per_sample(123)
            .expect("enabled TTL yields per-sample words");
        assert_eq!(words.len(), spp);
        for word in words {
            assert_eq!(word & !0xf, 0, "word {word:#x} escaped the 4-line mask");
        }
    }

    #[test]
    fn samples_for_packet_saturates_large_timestamps_without_panicking() {
        // L15/L16: a near-overflow packet must still produce a full block; the
        // sample index uses saturating arithmetic instead of wrapping/panicking.
        let backend = SimulatorBackend::default();
        let expected =
            backend.config.device.channel_count * backend.config.device.samples_per_packet;
        let data = backend.samples_for_packet(u64::MAX, u64::MAX - 1);
        assert_eq!(data.len(), expected);
    }

    #[test]
    fn spike_component_emits_biphasic_template_within_value_set() {
        // L15/L16: every spike sample is drawn from the fixed biphasic template,
        // and each firing emits the -180/260/-80 sequence on consecutive samples.
        let sample_rate = 8_000.0;
        let mut fired = 0;
        let mut index = 0u64;
        while index < 200_000 {
            let value = spike_component(DEFAULT_SIMULATOR_SEED, index, 0, sample_rate);
            assert!(
                matches!(value, 0 | -180 | 260 | -80),
                "unexpected spike value {value} at index {index}"
            );
            if value == -180 {
                fired += 1;
                assert_eq!(
                    spike_component(DEFAULT_SIMULATOR_SEED, index + 1, 0, sample_rate),
                    260
                );
                assert_eq!(
                    spike_component(DEFAULT_SIMULATOR_SEED, index + 2, 0, sample_rate),
                    -80
                );
            }
            index += 1;
        }
        assert!(fired > 0, "no spikes fired across the scanned window");
    }
}
