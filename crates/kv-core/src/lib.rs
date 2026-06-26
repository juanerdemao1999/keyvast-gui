//! Acquisition lifecycle orchestration for Keyvast.

pub mod pipeline;
pub mod process_metrics;

use std::fmt;

use kv_integrity::{IntegrityError, IntegrityReport, check_blocks_with_expected_start};
use kv_types::{AcquisitionState, DeviceConfig, DeviceStatus, SampleBlock};

pub trait AcquisitionSource {
    type Error: fmt::Display;

    fn read_block(&mut self) -> Result<SampleBlock, Self::Error>;
}

impl<F, E> AcquisitionSource for F
where
    F: FnMut() -> Result<SampleBlock, E>,
    E: fmt::Display,
{
    type Error = E;

    fn read_block(&mut self) -> Result<SampleBlock, Self::Error> {
        self()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AcquisitionRun {
    pub blocks: Vec<SampleBlock>,
    pub summary: AcquisitionRunSummary,
    pub integrity: IntegrityReport,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AcquisitionRunSummary {
    pub requested_blocks: u64,
    pub acquired_blocks: u64,
    pub sample_values: u64,
    pub state: AcquisitionState,
    pub state_history: Vec<AcquisitionState>,
    pub status: DeviceStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcquisitionConfigError {
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

impl fmt::Display for AcquisitionConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSampleRate => {
                write!(
                    formatter,
                    "sample rate must be finite and greater than zero"
                )
            }
            Self::EmptyChannelSet => write!(formatter, "channel count must be greater than zero"),
            Self::EmptyPacket => write!(formatter, "samples per packet must be greater than zero"),
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

impl std::error::Error for AcquisitionConfigError {}

#[derive(Debug, Clone, PartialEq)]
pub enum AcquisitionRunError {
    InvalidConfig {
        summary: Box<AcquisitionRunSummary>,
        reason: AcquisitionConfigError,
    },
    BackendRead {
        summary: Box<AcquisitionRunSummary>,
        message: String,
    },
    Integrity {
        summary: Box<AcquisitionRunSummary>,
        source: IntegrityError,
    },
}

impl AcquisitionRunError {
    pub fn summary(&self) -> &AcquisitionRunSummary {
        match self {
            Self::InvalidConfig { summary, .. }
            | Self::BackendRead { summary, .. }
            | Self::Integrity { summary, .. } => summary.as_ref(),
        }
    }
}

impl fmt::Display for AcquisitionRunError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig { reason, .. } => {
                write!(formatter, "acquisition config is invalid: {reason}")
            }
            Self::BackendRead { message, .. } => {
                write!(formatter, "backend read failed: {message}")
            }
            Self::Integrity { source, .. } => {
                write!(formatter, "acquisition integrity check failed: {source}")
            }
        }
    }
}

impl std::error::Error for AcquisitionRunError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidConfig { reason, .. } => Some(reason),
            Self::Integrity { source, .. } => Some(source),
            Self::BackendRead { .. } => None,
        }
    }
}

pub fn run_fixed_blocks<S>(
    config: &DeviceConfig,
    requested_blocks: usize,
    source: &mut S,
) -> Result<AcquisitionRun, AcquisitionRunError>
where
    S: AcquisitionSource,
{
    let requested_blocks_u64 = requested_blocks as u64;
    let mut state_history = vec![AcquisitionState::DeviceConnected];

    if let Err(reason) = validate_config(config) {
        let message = reason.to_string();
        state_history.push(AcquisitionState::Error);
        let summary = build_summary(
            config,
            requested_blocks_u64,
            &[],
            AcquisitionState::Error,
            state_history,
            StatusFlags {
                configured: false,
                acquiring: false,
            },
            Some(message),
        );
        return Err(AcquisitionRunError::InvalidConfig {
            summary: Box::new(summary),
            reason,
        });
    }

    state_history.push(AcquisitionState::Configured);
    state_history.push(AcquisitionState::Acquiring);

    let mut blocks = Vec::with_capacity(requested_blocks);
    for _ in 0..requested_blocks {
        match source.read_block() {
            Ok(block) => blocks.push(block),
            Err(error) => {
                let message = error.to_string();
                state_history.push(AcquisitionState::Error);
                let summary = build_summary(
                    config,
                    requested_blocks_u64,
                    &blocks,
                    AcquisitionState::Error,
                    state_history,
                    StatusFlags {
                        configured: true,
                        acquiring: false,
                    },
                    Some(message.clone()),
                );
                return Err(AcquisitionRunError::BackendRead {
                    summary: Box::new(summary),
                    message,
                });
            }
        }
    }

    state_history.push(AcquisitionState::Stopping);

    // Acquisition numbers packets from 0; anchor there so loss before the first
    // observed block is counted (DA43).
    let integrity = match check_blocks_with_expected_start(Some(0), &blocks) {
        Ok(report) => report,
        Err(source) => {
            let message = source.to_string();
            state_history.push(AcquisitionState::Error);
            let summary = build_summary(
                config,
                requested_blocks_u64,
                &blocks,
                AcquisitionState::Error,
                state_history,
                StatusFlags {
                    configured: true,
                    acquiring: false,
                },
                Some(message),
            );
            return Err(AcquisitionRunError::Integrity {
                summary: Box::new(summary),
                source,
            });
        }
    };

    state_history.push(AcquisitionState::Stopped);
    let summary = build_summary(
        config,
        requested_blocks_u64,
        &blocks,
        AcquisitionState::Stopped,
        state_history,
        StatusFlags {
            configured: true,
            acquiring: false,
        },
        None,
    );

    Ok(AcquisitionRun {
        blocks,
        summary,
        integrity,
    })
}

#[derive(Debug, Clone, Copy)]
struct StatusFlags {
    configured: bool,
    acquiring: bool,
}

fn validate_config(config: &DeviceConfig) -> Result<(), AcquisitionConfigError> {
    if !config.sample_rate.is_finite() || config.sample_rate <= 0.0 {
        return Err(AcquisitionConfigError::InvalidSampleRate);
    }

    if config.channel_count == 0 {
        return Err(AcquisitionConfigError::EmptyChannelSet);
    }

    if config.samples_per_packet == 0 {
        return Err(AcquisitionConfigError::EmptyPacket);
    }

    if config.ttl_line_count > u32::BITS as usize {
        return Err(AcquisitionConfigError::TtlLineCountOutOfRange {
            ttl_line_count: config.ttl_line_count,
        });
    }

    for &channel in &config.enabled_channels {
        if channel >= config.channel_count {
            return Err(AcquisitionConfigError::EnabledChannelOutOfRange {
                channel,
                channel_count: config.channel_count,
            });
        }
    }

    Ok(())
}

fn build_summary(
    config: &DeviceConfig,
    requested_blocks: u64,
    blocks: &[SampleBlock],
    state: AcquisitionState,
    state_history: Vec<AcquisitionState>,
    flags: StatusFlags,
    last_error: Option<String>,
) -> AcquisitionRunSummary {
    AcquisitionRunSummary {
        requested_blocks,
        acquired_blocks: blocks.len() as u64,
        sample_values: sample_values(blocks),
        state,
        state_history,
        status: build_status(config, blocks, flags, last_error),
    }
}

fn build_status(
    config: &DeviceConfig,
    blocks: &[SampleBlock],
    flags: StatusFlags,
    last_error: Option<String>,
) -> DeviceStatus {
    let last_block = blocks.last();

    DeviceStatus {
        device_id: config.device_id.clone(),
        backend: config.backend,
        connected: true,
        configured: flags.configured,
        acquiring: flags.acquiring,
        sample_rate: config.sample_rate,
        channel_count: config.channel_count,
        packet_rate_hz: packet_rate_hz(config),
        last_packet_id: last_block.map(|block| block.packet_id),
        ttl_bits: last_block.map(|block| block.ttl_bits).unwrap_or_default(),
        last_error,
    }
}

fn sample_values(blocks: &[SampleBlock]) -> u64 {
    blocks
        .iter()
        .map(|block| block.data.len() as u64)
        .sum::<u64>()
}

fn packet_rate_hz(config: &DeviceConfig) -> f64 {
    if !config.sample_rate.is_finite()
        || config.sample_rate <= 0.0
        || config.samples_per_packet == 0
    {
        return 0.0;
    }

    config.sample_rate / config.samples_per_packet as f64
}
