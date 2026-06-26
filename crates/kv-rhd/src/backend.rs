use std::fmt;
use std::path::PathBuf;

use kv_types::SampleBlock;

use crate::commands::RhdCommandError;
use crate::frontpanel::{FrontPanelError, FrontPanelLibrary};
use crate::impedance;
use crate::parser::{RhythmParseError, parse_rhythm_data_block};
use crate::protocol::{
    CHANNELS_PER_STREAM, DEFAULT_CABLE_LENGTH_METERS, DEFAULT_RHD_DEVICE_ID,
    DEFAULT_RHD_SAMPLE_RATE, RhythmConfigError, RhythmDataConfig, SAMPLES_PER_USB_BLOCK,
};
use crate::rhythm_board::RhythmFrontPanelBoard;

#[derive(Debug, Clone, PartialEq)]
pub struct RhdHardwareOptions {
    pub bitfile_path: PathBuf,
    pub frontpanel_dll_path: Option<PathBuf>,
    pub serial: Option<String>,
    pub data: RhythmDataConfig,
    pub cable_length_meters: f64,
}

impl RhdHardwareOptions {
    pub fn new(bitfile_path: impl Into<PathBuf>, enabled_streams: usize) -> Self {
        Self {
            bitfile_path: bitfile_path.into(),
            frontpanel_dll_path: None,
            serial: None,
            data: RhythmDataConfig {
                device_id: DEFAULT_RHD_DEVICE_ID.to_string(),
                stream_id: 0,
                enabled_streams,
                sample_rate: DEFAULT_RHD_SAMPLE_RATE,
                samples_per_block: SAMPLES_PER_USB_BLOCK,
            },
            cable_length_meters: DEFAULT_CABLE_LENGTH_METERS,
        }
    }
}

pub struct RhdHardwareBackend {
    board: RhythmFrontPanelBoard,
    config: RhythmDataConfig,
    next_packet_id: u64,
    acquisition_started: bool,
    logged_first_block: bool,
}

impl RhdHardwareBackend {
    pub fn open(options: RhdHardwareOptions) -> Result<Self, RhdReadError> {
        log::info!(
            "opening RHD backend: bitfile={}, streams={}",
            options.bitfile_path.display(),
            options.data.enabled_streams
        );
        options
            .data
            .validate()
            .map_err(RhdReadError::InvalidConfig)?;

        let library = FrontPanelLibrary::load(options.frontpanel_dll_path.clone())
            .map_err(RhdReadError::FrontPanel)?;
        let device = library
            .open_device(options.serial.as_deref())
            .map_err(RhdReadError::FrontPanel)?;
        let (board, detected_streams) = RhythmFrontPanelBoard::configure(
            device,
            &options.bitfile_path,
            options.data.enabled_streams,
            options.cable_length_meters,
        )?;

        let mut config = options.data;
        if detected_streams != config.enabled_streams {
            log::info!(
                "auto-detect: using detected {} stream(s) / {} channels instead of the \
                 requested {} stream(s)",
                detected_streams,
                detected_streams * CHANNELS_PER_STREAM,
                config.enabled_streams,
            );
            config.enabled_streams = detected_streams;
        }

        log::info!("RHD backend ready");
        Ok(Self {
            board,
            config,
            next_packet_id: 0,
            acquisition_started: false,
            logged_first_block: false,
        })
    }

    pub fn read_block(&mut self) -> Result<SampleBlock, RhdReadError> {
        if !self.acquisition_started {
            self.board.start_continuous_acquisition()?;
            self.acquisition_started = true;
            log::info!("continuous acquisition started; reading first block...");
        }

        let raw = self.board.read_raw_block(&self.config)?;
        let packet_id = self.next_packet_id;
        self.next_packet_id = self.next_packet_id.saturating_add(1);
        let mut block =
            parse_rhythm_data_block(packet_id, &raw, &self.config).map_err(RhdReadError::Parse)?;
        // Stamp the host wall-clock at the moment the live block arrived so the
        // FPGA sample counter (`timestamp_start`) can be aligned to wall-clock
        // time and host↔FPGA drift estimated offline (DA16).
        block.host_time_ns = Some(kv_types::host_time_ns_now());

        if !self.logged_first_block {
            self.logged_first_block = true;
            let (min, max) = block
                .data
                .iter()
                .fold((i16::MAX, i16::MIN), |(lo, hi), &value| {
                    (lo.min(value), hi.max(value))
                });
            let note = if min == max {
                "  <-- FLAT: no SPI response. Check the headstage is connected & powered, and \
                 that this is a KeyVast bitstream — the stock Intan bit cannot drive the KeyVast \
                 headstage SPI pins (it reads 0xFFFF on every port)."
            } else {
                ""
            };
            log::info!(
                "first block OK: {} channels x {} samples, raw amplifier i16 min={min} max={max}{note}",
                block.channel_count,
                block.samples_per_channel
            );
        }

        Ok(block)
    }

    /// Run an impedance measurement across all channels.
    ///
    /// The test drives the SPI bus itself, so it requires exclusive device
    /// access — call this on a freshly opened backend, not while continuous
    /// acquisition is streaming.
    pub fn run_impedance_test(
        &self,
        config: &impedance::ImpedanceTestConfig,
        progress_callback: Option<&dyn Fn(usize, usize)>,
    ) -> Result<impedance::ImpedanceResult, RhdReadError> {
        self.board
            .run_impedance_test(config, self.config.enabled_streams, progress_callback)
    }
}

#[derive(Debug)]
pub enum RhdReadError {
    InvalidConfig(RhythmConfigError),
    Command(RhdCommandError),
    FrontPanel(FrontPanelError),
    Parse(RhythmParseError),
    UnexpectedBoardId { expected: u32, observed: u32 },
    InvalidPort { port_index: usize },
    NotEnoughFifoWords { needed: u32, available: u32 },
    ShortPipeRead { expected: usize, observed: usize },
    SpiStillRunning,
    PllDcmTimeout,
    PllLockTimeout,
    FifoFlushIncomplete { remaining_words: u32 },
    HeadstageNotFound,
    HalfScaleAmplifierData { mean_raw_word: u32 },
    Cancelled,
}

impl fmt::Display for RhdReadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig(error) => write!(formatter, "{error}"),
            Self::Command(error) => write!(formatter, "{error}"),
            Self::FrontPanel(error) => write!(formatter, "{error}"),
            Self::Parse(error) => write!(formatter, "{error}"),
            Self::UnexpectedBoardId { expected, observed } => write!(
                formatter,
                "unexpected Rhythm board id: expected {expected}, observed {observed}"
            ),
            Self::InvalidPort { port_index } => {
                write!(formatter, "invalid Rhythm SPI port index {port_index}")
            }
            Self::NotEnoughFifoWords { needed, available } => write!(
                formatter,
                "not enough words in Rhythm FIFO: needed {needed}, available {available}"
            ),
            Self::ShortPipeRead { expected, observed } => write!(
                formatter,
                "short Rhythm pipe read: expected {expected} bytes, observed {observed}"
            ),
            Self::SpiStillRunning => {
                write!(formatter, "Rhythm SPI run did not stop before timeout")
            }
            Self::PllDcmTimeout => write!(formatter, "PLL DCM did not complete before timeout"),
            Self::PllLockTimeout => write!(formatter, "PLL data clock failed to lock"),
            Self::FifoFlushIncomplete { remaining_words } => write!(
                formatter,
                "FIFO flush incomplete: {remaining_words} words remaining"
            ),
            Self::HeadstageNotFound => write!(
                formatter,
                "no responding RHD headstage found on any SPI port; check it is connected and powered"
            ),
            Self::HalfScaleAmplifierData { mean_raw_word } => write!(
                formatter,
                "amplifier data is half-scale (mean raw word 0x{mean_raw_word:04x} ~ 0x4000): \
                 wrong MISO sampling phase"
            ),
            Self::Cancelled => write!(formatter, "cancelled by Ctrl-C"),
        }
    }
}

impl std::error::Error for RhdReadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidConfig(error) => Some(error),
            Self::Command(error) => Some(error),
            Self::FrontPanel(error) => Some(error),
            Self::Parse(error) => Some(error),
            Self::UnexpectedBoardId { .. }
            | Self::InvalidPort { .. }
            | Self::NotEnoughFifoWords { .. }
            | Self::ShortPipeRead { .. }
            | Self::SpiStillRunning
            | Self::PllDcmTimeout
            | Self::PllLockTimeout
            | Self::FifoFlushIncomplete { .. }
            | Self::HeadstageNotFound
            | Self::HalfScaleAmplifierData { .. }
            | Self::Cancelled => None,
        }
    }
}
