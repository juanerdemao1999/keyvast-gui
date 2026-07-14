use std::fmt;

use kv_types::{DEFAULT_TTL_LINE_COUNT, DeviceBackendKind, DeviceConfig};

pub const RHYTHM_HEADER_MAGIC: u64 = 0xd7a2_2aaa_3813_2a53;

/// Number of TTL digital input lines the Rhythm frame carries (one 16-bit TTL
/// word per sample). Matches `kv_types::DEFAULT_TTL_LINE_COUNT`.
pub const RHYTHM_TTL_LINE_COUNT: usize = DEFAULT_TTL_LINE_COUNT;
pub const RHYTHM_BOARD_ID: u32 = 700;
pub const CHANNELS_PER_STREAM: usize = 32;
pub const SAMPLES_PER_USB_BLOCK: usize = 256;
pub const MAX_SUPPORTED_STREAMS: usize = 2;
pub const DEFAULT_RHD_SAMPLE_RATE: f64 = 30_000.0;
pub const USB3_BLOCK_SIZE_BYTES: usize = 1024;
pub const RHD_AMPLIFIER_MICROVOLTS_PER_COUNT: f32 = 0.195;

/// On-chip DAC reference voltage (volts) used by the RHD impedance-check
/// current source. The peak DAC output is `128 * RHD_DAC_VREF_VOLTS / 256`.
pub const RHD_DAC_VREF_VOLTS: f64 = 1.225;

/// Number of auxiliary result words per stream per sample.
pub const AUX_CHANNELS_PER_STREAM: usize = 3;

/// Board ADC channels in each frame.
pub const BOARD_ADC_CHANNELS: usize = 8;

/// Open Ephys scale factor for VDD supply voltage: 0.0000748 V/count.
#[allow(dead_code)] // hardware bring-up reference
pub const RHD_VDD_VOLTS_PER_COUNT: f64 = 0.0000748;

/// Open Ephys scale factor for auxiliary ADC inputs: 0.0000374 V/count.
#[allow(dead_code)] // hardware bring-up reference
pub const RHD_AUX_ADC_VOLTS_PER_COUNT: f64 = 0.0000374;

/// Default device-ID string for Opal Kelly XEM7310 + Rhythm FPGA.
pub const DEFAULT_RHD_DEVICE_ID: &str = "rhd-xem7310";

/// Canonical, ordered list of FPGA bitfile names this backend can drive,
/// most-preferred first. Single source of truth shared by the CLI default
/// and the GUI's best-effort bitfile picker so the three components no longer
/// disagree on which bitstreams exist.
///
/// On the KeyVast PCB the 8 RHD SPI buses are re-routed through the module-IO
/// ring, so only a KeyVast bitstream (`keyvast_*`) reaches the headstage; the
/// stock Intan build is kept as a last-resort fallback for a genuine Intan
/// recording controller.
pub const RHD_BITFILE_CANDIDATES: [&str; 3] = [
    "keyvast_combined_download.bit",
    DEFAULT_RHD_BITFILE_NAME,
    "intan_rec_controller_7310.bit",
];

/// Default bitfile name used by the headless CLI smoke test (the UART-enabled
/// KeyVast build). Also the second GUI candidate via [`RHD_BITFILE_CANDIDATES`].
pub const DEFAULT_RHD_BITFILE_NAME: &str = "keyvast_260607_with_UART.bit";

/// Default SPI cable length in meters (3 ft ≈ 0.9144 m).
pub const DEFAULT_CABLE_LENGTH_METERS: f64 = 0.9144;

/// RHD2132 16-channel headstage: amplifier channels are offset by this
/// many channels from channel 0. The chip only populates the upper 16
/// of its 32 logical amplifier channels.
#[allow(dead_code)] // hardware bring-up reference
pub const RHD2132_16CH_OFFSET: usize = 16;

/// Supported RHD amplifier chip types.
#[allow(dead_code)] // hardware bring-up reference
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RhdChipType {
    /// RHD2132 — 32 channels, single MISO.
    Rhd2132,
    /// RHD2132 in 16-channel mode — only channels 16..31 are active.
    Rhd2132_16ch,
    /// RHD2216 — 16 channels, single MISO.
    Rhd2216,
    /// RHD2164 — 64 channels, dual MISO (two streams per headstage).
    Rhd2164,
}

#[allow(dead_code)] // hardware bring-up reference
impl RhdChipType {
    /// Identify the chip type from the register-63 chip-ID readback. The
    /// RHD2000 ROM register 63 holds the chip ID as a literal value (matching
    /// Open Ephys `getDeviceId`): 1 = RHD2132 (32ch), 2 = RHD2216 (16ch),
    /// 4 = RHD2164 (64ch).
    pub fn from_register63(reg63: u16) -> Option<Self> {
        match reg63 & 0xff {
            1 => Some(Self::Rhd2132),
            2 => Some(Self::Rhd2216),
            4 => Some(Self::Rhd2164),
            _ => None,
        }
    }

    /// Number of amplifier channels the chip exposes.
    pub fn channel_count(self) -> usize {
        match self {
            Self::Rhd2132 => 32,
            Self::Rhd2132_16ch => 16,
            Self::Rhd2216 => 16,
            Self::Rhd2164 => 64,
        }
    }

    /// Number of data streams per headstage.
    pub fn streams_per_headstage(self) -> usize {
        match self {
            Self::Rhd2164 => 2,
            _ => 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RhythmDataConfig {
    pub device_id: String,
    pub stream_id: u32,
    pub enabled_streams: usize,
    pub sample_rate: f64,
    pub samples_per_block: usize,
}

impl RhythmDataConfig {
    pub fn two_headstages(device_id: impl Into<String>) -> Self {
        Self {
            device_id: device_id.into(),
            stream_id: 0,
            enabled_streams: 2,
            sample_rate: DEFAULT_RHD_SAMPLE_RATE,
            samples_per_block: SAMPLES_PER_USB_BLOCK,
        }
    }

    pub fn channel_count(&self) -> usize {
        self.enabled_streams.saturating_mul(CHANNELS_PER_STREAM)
    }

    pub fn device_config(&self) -> Result<DeviceConfig, RhythmConfigError> {
        validate_stream_count(self.enabled_streams)?;
        validate_samples_per_block(self.samples_per_block)?;
        validate_sample_rate(self.sample_rate)?;

        let channel_count = self.channel_count();
        Ok(DeviceConfig {
            device_id: self.device_id.clone(),
            // Transport kind is fixed to USB because the only Rhythm bring-up
            // path is the Opal Kelly XEM7310 USB3 board; revisit if a non-USB
            // transport is confirmed (project rule 1).
            backend: DeviceBackendKind::Usb,
            sample_rate: self.sample_rate,
            channel_count,
            samples_per_packet: self.samples_per_block,
            enabled_channels: (0..channel_count).collect(),
            // The Rhythm frame always carries one TTL word per sample, so the
            // digital inputs are always present at the protocol level.
            ttl_enabled: true,
            ttl_line_count: RHYTHM_TTL_LINE_COUNT,
        })
    }

    pub fn validate(&self) -> Result<(), RhythmConfigError> {
        self.device_config().map(|_| ())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RhythmConfigError {
    InvalidStreamCount { enabled_streams: usize },
    InvalidSamplesPerBlock { samples_per_block: usize },
    InvalidSampleRate,
}

impl fmt::Display for RhythmConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidStreamCount { enabled_streams } => write!(
                formatter,
                "enabled RHD stream count must be 1..={MAX_SUPPORTED_STREAMS}, got {enabled_streams}"
            ),
            Self::InvalidSamplesPerBlock { samples_per_block } => write!(
                formatter,
                "RHD samples per USB block must be greater than zero, got {samples_per_block}"
            ),
            Self::InvalidSampleRate => write!(
                formatter,
                "RHD sample rate must be finite and greater than zero"
            ),
        }
    }
}

impl std::error::Error for RhythmConfigError {}

pub fn validate_stream_count(enabled_streams: usize) -> Result<(), RhythmConfigError> {
    if enabled_streams == 0 || enabled_streams > MAX_SUPPORTED_STREAMS {
        return Err(RhythmConfigError::InvalidStreamCount { enabled_streams });
    }

    Ok(())
}

pub fn validate_samples_per_block(samples_per_block: usize) -> Result<(), RhythmConfigError> {
    if samples_per_block == 0 {
        return Err(RhythmConfigError::InvalidSamplesPerBlock { samples_per_block });
    }

    Ok(())
}

pub fn validate_sample_rate(sample_rate: f64) -> Result<(), RhythmConfigError> {
    if !sample_rate.is_finite() || sample_rate <= 0.0 {
        return Err(RhythmConfigError::InvalidSampleRate);
    }

    Ok(())
}

/// Bit mask with the low `enabled_streams` bits set.
///
/// Guards the shift so an `enabled_streams >= u32::BITS` can never trigger a
/// shift-overflow panic (returns a full mask in that degenerate case).
#[must_use]
pub fn stream_enable_mask(enabled_streams: usize) -> u32 {
    if enabled_streams >= u32::BITS as usize {
        u32::MAX
    } else {
        (1_u32 << enabled_streams) - 1
    }
}

/// Word/byte layout of a single Rhythm USB data frame for a given number of
/// enabled streams.
///
/// This is the single source of truth for the per-frame arithmetic. The parser
/// and every MISO-scan / impedance analysis helper derive their offsets from
/// here instead of re-deriving the magic/timestamp/aux/amp/filler layout inline,
/// so a protocol change is a one-line edit rather than a six-way hunt.
///
/// Word order within a frame (each unit is one 16-bit word unless noted):
/// `magic(4) | timestamp(2) | aux[aux_ch][stream] (3*streams) |
///  amp[channel][stream] (32*streams) | filler(streams) | board_adc(8) | ttl_in(1) | ttl_out(1)`.
#[derive(Debug, Clone, Copy)]
pub struct FrameLayout {
    enabled_streams: usize,
}

impl FrameLayout {
    pub fn new(enabled_streams: usize) -> Self {
        Self { enabled_streams }
    }

    /// Filler words padding the body to a multiple of 4: **`streams % 4`**.
    ///
    /// This is the gateware's contract, and it is Intan's:
    /// `rhd2000datablockusb3.cpp:118` computes the frame as
    /// `4 + 2 + streams*(32+3) + (streams % 4) + 8 + 2`, and our FPGA emits
    /// `fillN = CountOne(streamEn)(1 downto 0)` (`RhdOkShim.scala:207`) — the low
    /// two bits, i.e. `streams % 4`. Measured on a live XEM7310 + RHD2132: the
    /// frame stride is exactly 52 words at 1 stream.
    ///
    /// Three formulas have shipped, and each is wrong somewhere. Pinned by
    /// `filler_matches_gateway_contract` below so this cannot drift again:
    ///
    /// | streams | `% 4` (correct) | `(4 - s%4) % 4` | `s` (per-stream) |
    /// |---------|-----------------|-----------------|------------------|
    /// | 1       | **1**           | 3  ✗            | 1                |
    /// | 2       | **2**           | 2               | 2                |
    /// | 3       | **3**           | 1  ✗            | 3                |
    /// | 4       | **0**           | 0               | 4  ✗             |
    /// | 8       | **0**           | 0               | 8  ✗             |
    ///
    /// They all agree at 2 streams, which is why both wrong versions stayed
    /// latent: `(4 - s%4) % 4` broke the single-headstage case (expected 54-word
    /// frames, FPGA sends 52 -> "needed 13824, available 13312"), and the
    /// per-stream model is correct at 1-3 streams but diverges at every multiple
    /// of 4 (at 4 streams it expects 4 filler words where the FPGA emits none).
    pub fn filler_words(&self) -> usize {
        self.enabled_streams % 4
    }

    /// Total number of 16-bit words in one frame.
    pub fn words_per_frame(&self) -> usize {
        4 + 2
            + self.enabled_streams * (CHANNELS_PER_STREAM + AUX_CHANNELS_PER_STREAM)
            + self.filler_words()
            + BOARD_ADC_CHANNELS
            + 2
    }

    /// Total number of bytes in one frame.
    pub fn bytes_per_frame(&self) -> usize {
        self.words_per_frame() * 2
    }

    /// Word offset of the first aux-command word (aux_ch 0, stream 0).
    fn aux_base_words(&self) -> usize {
        4 + 2
    }

    /// Word offset of the AuxCmd3 result word (aux_ch index 2) for `stream`.
    /// Aux words are aux_ch-major, stream-minor, matching `parse_rhythm_data_block`.
    pub fn auxcmd3_word_offset(&self, stream: usize) -> usize {
        self.aux_base_words() + 2 * self.enabled_streams + stream
    }

    /// Word offset of the amplifier sample for intra-stream channel `intra_ch`
    /// on `stream`. Amplifier words are channel-major, stream-minor.
    pub fn amp_word_offset(&self, intra_ch: usize, stream: usize) -> usize {
        self.aux_base_words()
            + AUX_CHANNELS_PER_STREAM * self.enabled_streams
            + intra_ch * self.enabled_streams
            + stream
    }

    /// Byte offset of a frame-relative word at sample index `sample`.
    pub fn word_byte_offset(&self, sample: usize, word_in_frame: usize) -> usize {
        sample * self.bytes_per_frame() + word_in_frame * 2
    }
}

pub fn words_per_frame(enabled_streams: usize) -> Result<usize, RhythmConfigError> {
    validate_stream_count(enabled_streams)?;

    Ok(FrameLayout::new(enabled_streams).words_per_frame())
}

pub fn bytes_per_block(
    enabled_streams: usize,
    samples_per_block: usize,
) -> Result<usize, RhythmConfigError> {
    validate_samples_per_block(samples_per_block)?;

    Ok(words_per_frame(enabled_streams)?
        .saturating_mul(samples_per_block)
        .saturating_mul(2))
}

pub fn raw_word_to_signed_count(word: u16) -> i16 {
    (word as i32 - 32_768) as i16
}

pub fn signed_count_to_microvolts(count: i16) -> f32 {
    count as f32 * RHD_AMPLIFIER_MICROVOLTS_PER_COUNT
}

#[cfg(test)]
mod frame_contract_tests {
    use super::*;

    /// The canonical Rhythm frame size, from the two authorities that agree:
    ///   * Intan  `rhd2000datablockusb3.cpp:118`
    ///     `4 + 2 + streams*(CHANNELS_PER_STREAM + 3) + (streams % 4) + 8 + 2`
    ///   * KeyVast gateware `RhdOkShim.scala:207,293`
    ///     `fillN = CountOne(streamEn)(1 downto 0)`  (= streams % 4)
    ///
    /// Hardware-measured: 52-word frames at 1 stream (XEM7310 + RHD2132).
    fn canonical_words_per_frame(streams: usize) -> usize {
        4 + 2
            + streams * (CHANNELS_PER_STREAM + AUX_CHANNELS_PER_STREAM)
            + (streams % 4)
            + BOARD_ADC_CHANNELS
            + 2
    }

    /// The host's frame arithmetic MUST equal the gateware's, at every stream
    /// count the gateware can emit — not just the ones we ship today.
    ///
    /// This is the regression that three separate implementations failed. The
    /// parser demands the magic and a monotonic timestamp on EVERY sample, so a
    /// frame-size mismatch is not graceful degradation: it hard-fails on the
    /// first frame. Do not relax this test to the supported range; the whole
    /// point is that it catches drift BEFORE a wider headstage config is enabled.
    #[test]
    fn filler_matches_gateware_contract() {
        for streams in 1..=32 {
            let layout = FrameLayout::new(streams);
            assert_eq!(
                layout.filler_words(),
                streams % 4,
                "filler at {streams} stream(s) must be `streams % 4` (gateware: fillN)"
            );
            assert_eq!(
                layout.words_per_frame(),
                canonical_words_per_frame(streams),
                "words/frame at {streams} stream(s) disagrees with the gateware contract"
            );
        }
    }

    /// Spot-check the values that actually burned us, so a future refactor that
    /// "simplifies" the formula has to confront them explicitly.
    #[test]
    fn known_frame_sizes() {
        // 1 stream = one RHD2132 = the single-headstage case, measured on hardware.
        assert_eq!(FrameLayout::new(1).words_per_frame(), 52);
        // 2 streams: where all three wrong formulas coincidentally agree.
        assert_eq!(FrameLayout::new(2).words_per_frame(), 88);
        // 4 streams: filler is 0, NOT 4 — where the per-stream model breaks.
        assert_eq!(FrameLayout::new(4).filler_words(), 0);
        assert_eq!(FrameLayout::new(4).words_per_frame(), 156);
        // 8 streams: filler 0 again.
        assert_eq!(FrameLayout::new(8).words_per_frame(), 296);
    }
}
