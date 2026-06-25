//! Intan RHD / Opal Kelly Rhythm USB3 acquisition support.

mod backend;
mod commands;
mod frame_analysis;
mod frontpanel;
pub mod impedance;
mod parser;
mod protocol;
mod rhythm_acquire;
mod rhythm_board;

pub use backend::{RhdHardwareBackend, RhdHardwareOptions, RhdReadError};
pub use commands::{
    AuxCommandSlot, BoardPort, MAX_COMMAND_LENGTH, RHD_ADC_CALIBRATION_SAMPLES,
    RHD_COMMAND_LIST_LEN, Rhd2000CommandType, Rhd2000Registers, RhdCommandError, ZcheckScale,
    create_rhd2000_command,
};
pub use frontpanel::{FrontPanelError, default_frontpanel_dll_path};
pub use impedance::{
    ChannelImpedance, ImpedanceError, ImpedanceResult, ImpedanceTestConfig, auto_select_scale,
    compute_impedance,
};
pub use parser::{RhythmParseError, parse_rhythm_data_block};
pub use protocol::{
    CHANNELS_PER_STREAM, DEFAULT_CABLE_LENGTH_METERS, DEFAULT_RHD_BITFILE_NAME,
    DEFAULT_RHD_DEVICE_ID, DEFAULT_RHD_SAMPLE_RATE, RHD_AMPLIFIER_MICROVOLTS_PER_COUNT,
    RHD_BITFILE_CANDIDATES, RHYTHM_BOARD_ID, RhdChipType, RhythmConfigError, RhythmDataConfig,
    SAMPLES_PER_USB_BLOCK, USB3_BLOCK_SIZE_BYTES, bytes_per_block, raw_word_to_signed_count,
    signed_count_to_microvolts, words_per_frame,
};
