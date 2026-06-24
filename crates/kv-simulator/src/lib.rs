//! Simulator backend for hardware-independent acquisition tests.

use std::fmt;

use kv_types::{DeviceBackendKind, DeviceConfig, SampleBlock, SampleBlockError};

pub const DEFAULT_SIMULATOR_SEED: u64 = 0x4b56_5354_0000_0001;

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
            self.next_packet_id = self.next_packet_id.saturating_add(1);
        }

        let packet_id = self.next_packet_id;
        let timestamp_start =
            packet_id.saturating_mul(self.config.device.samples_per_packet as u64);
        let block = SampleBlock {
            device_id: self.config.device.device_id.clone(),
            stream_id: self.config.stream_id,
            packet_id,
            timestamp_start,
            sample_rate: self.config.device.sample_rate,
            channel_count: self.config.device.channel_count,
            samples_per_channel: self.config.device.samples_per_packet,
            ttl_bits: self.ttl_bits(packet_id),
            data: self.samples_for_packet(packet_id, timestamp_start),
            aux_data: None,
            board_adc_data: None,
            ttl_in_per_sample: None,
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
        let noise_seed = self.config.seed
            ^ packet_id.rotate_left(13)
            ^ sample_index.rotate_left(7)
            ^ (channel as u64).rotate_left(29);
        let noise = (mix_u64(noise_seed) % 41) as i32 - 20;
        let lfp = triangle_wave(sample_index.saturating_add((channel as u64).saturating_mul(3)));
        let spike = spike_component(self.config.seed, sample_index, channel);
        clamp_i16(noise + lfp + spike)
    }

    fn ttl_bits(&self, packet_id: u64) -> u32 {
        if !self.config.device.ttl_enabled || self.config.device.ttl_line_count == 0 {
            return 0;
        }

        let mask = if self.config.device.ttl_line_count == u32::BITS as usize {
            u32::MAX
        } else {
            (1_u32 << self.config.device.ttl_line_count) - 1
        };

        (mix_u64(self.config.seed ^ (packet_id / 8)) as u32) & mask
    }
}

impl Default for SimulatorBackend {
    fn default() -> Self {
        // SAFETY: SimulatorConfig::default() always passes validate_device_config().
        // Use unwrap_or_else to avoid a bare `expect` in library code while
        // still documenting the invariant with a clear message.
        match Self::new(SimulatorConfig::default()) {
            Ok(backend) => backend,
            Err(e) => unreachable!("default SimulatorConfig must be valid: {e}"),
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

/// Slow triangle wave simulating an LFP-like oscillation (~7.5 Hz at 30 kHz).
fn triangle_wave(sample_index: u64) -> i32 {
    // Period in samples: 30000 / 7.5 = 4000 samples ≈ 7.5 Hz LFP at 30 kHz.
    const LFP_PERIOD: u64 = 4000;
    let phase = (sample_index % LFP_PERIOD) as i32;
    let half_period = (LFP_PERIOD / 2) as i32;

    if phase < half_period {
        phase - (half_period / 2)
    } else {
        (LFP_PERIOD as i32 - phase) - (half_period / 2)
    }
}

fn spike_component(seed: u64, sample_index: u64, channel: usize) -> i32 {
    // Channel-dependent spike rate: lower channels spike more often.
    // Modulus controls rarity — higher modulus = fewer spikes.
    let rarity = 512 + (channel as u64 % 8) * 128;

    // Use per-sample seed so spikes don't burst across an entire packet.
    let spike_phase = sample_index % 3;
    let spike_start = sample_index - spike_phase;
    let event_seed = seed ^ spike_start ^ (channel as u64 * 17);
    if mix_u64(event_seed).is_multiple_of(rarity) && spike_phase <= 2 {
        // 3-sample biphasic spike template
        match spike_phase {
            0 => -180,
            1 => 260,
            2 => -80,
            _ => 0,
        }
    } else {
        0
    }
}

fn clamp_i16(value: i32) -> i16 {
    value.clamp(i16::MIN as i32, i16::MAX as i32) as i16
}
