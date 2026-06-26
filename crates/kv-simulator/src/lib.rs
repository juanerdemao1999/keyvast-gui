//! Simulator backend for hardware-independent acquisition tests.

use std::fmt;

use kv_types::{DeviceBackendKind, DeviceConfig, DeviceConfigError, SampleBlock, SampleBlockError};

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
}

impl Default for SimulatorConfig {
    fn default() -> Self {
        Self {
            device: DeviceConfig::simulator_default(),
            seed: DEFAULT_SIMULATOR_SEED,
            stream_id: 0,
            drop_packet_ids: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SimulatorBackend {
    config: SimulatorConfig,
    next_packet_id: u64,
}

impl SimulatorBackend {
    pub fn new(mut config: SimulatorConfig) -> Result<Self, SimulatorConfigError> {
        validate_device_config(&config.device)?;
        config.drop_packet_ids.sort_unstable();
        config.drop_packet_ids.dedup();

        Ok(Self {
            config,
            next_packet_id: 0,
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
        };

        block
            .validate_against_ttl_lines(self.config.device.ttl_line_count)
            .map_err(SimulatorError::InvalidGeneratedBlock)?;
        self.next_packet_id = self.next_packet_id.saturating_add(1);

        Ok(block)
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

impl From<DeviceConfigError> for SimulatorConfigError {
    fn from(error: DeviceConfigError) -> Self {
        match error {
            DeviceConfigError::InvalidSampleRate => Self::InvalidSampleRate,
            DeviceConfigError::EmptyChannelSet => Self::EmptyChannelSet,
            DeviceConfigError::EmptyPacket => Self::EmptyPacket,
            DeviceConfigError::TtlLineCountOutOfRange { ttl_line_count } => {
                Self::TtlLineCountOutOfRange { ttl_line_count }
            }
            DeviceConfigError::EnabledChannelOutOfRange {
                channel,
                channel_count,
            } => Self::EnabledChannelOutOfRange {
                channel,
                channel_count,
            },
        }
    }
}

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
    // The simulator backend constraint is specific to this backend; the rest of
    // the structural checks are shared with every other backend through the
    // type-level `DeviceConfig::validate` (DA30).
    if config.backend != DeviceBackendKind::Simulator {
        return Err(SimulatorConfigError::NonSimulatorBackend {
            backend: config.backend,
        });
    }

    config.validate().map_err(SimulatorConfigError::from)
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
