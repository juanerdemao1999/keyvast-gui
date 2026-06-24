use std::{
    fmt,
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

use kv_types::SampleBlock;

use crate::{
    commands::{
        AuxCommandSlot, BoardPort, RHD_ADC_CALIBRATION_SAMPLES, Rhd2000Registers, RhdCommandError,
    },
    frontpanel::{FrontPanelDevice, FrontPanelError, FrontPanelLibrary},
    impedance,
    parser::{RhythmParseError, parse_rhythm_data_block},
    protocol::{
        CHANNELS_PER_STREAM, DEFAULT_CABLE_LENGTH_METERS, DEFAULT_RHD_DEVICE_ID,
        DEFAULT_RHD_SAMPLE_RATE, RHYTHM_BOARD_ID, RhythmConfigError, RhythmDataConfig,
        SAMPLES_PER_USB_BLOCK, USB3_BLOCK_SIZE_BYTES, bytes_per_block,
    },
};

const WIRE_IN_RESET_RUN: i32 = 0x00;
const WIRE_IN_MAX_TIME_STEP_LSB: i32 = 0x01;
const WIRE_IN_MAX_TIME_STEP_MSB: i32 = 0x02;
const WIRE_IN_DATA_FREQ_PLL: i32 = 0x03;
const WIRE_IN_MISO_DELAY: i32 = 0x04;
const WIRE_IN_CMD_RAM_ADDR: i32 = 0x05;
const WIRE_IN_CMD_RAM_BANK: i32 = 0x06;
const WIRE_IN_CMD_RAM_DATA: i32 = 0x07;
const WIRE_IN_AUX_CMD_BANK1: i32 = 0x08;
const WIRE_IN_AUX_CMD_BANK2: i32 = 0x09;
const WIRE_IN_AUX_CMD_BANK3: i32 = 0x0a;
const WIRE_IN_AUX_CMD_LENGTH: i32 = 0x0b;
const WIRE_IN_AUX_CMD_LOOP: i32 = 0x0c;
const WIRE_IN_DATA_STREAM_SEL_1234: i32 = 0x12;
const WIRE_IN_DATA_STREAM_SEL_5678: i32 = 0x13;
const WIRE_IN_DATA_STREAM_EN: i32 = 0x14;
const WIRE_IN_TTL_OUT: i32 = 0x15;
const WIRE_IN_MULTI_USE: i32 = 0x1f;
const WIRE_OUT_NUM_WORDS_LSB: i32 = 0x20;
const WIRE_OUT_NUM_WORDS_MSB: i32 = 0x26;
const WIRE_OUT_SPI_RUNNING: i32 = 0x22;
const WIRE_OUT_DATA_CLK_LOCKED: i32 = 0x24;
const WIRE_OUT_BOARD_ID: i32 = 0x3e;
const WIRE_OUT_BOARD_VERSION: i32 = 0x3f;
const TRIG_IN_CONFIG: i32 = 0x40;
const TRIG_IN_SPI_START: i32 = 0x41;
const PIPE_OUT_DATA: i32 = 0xa0;
const RAM_BURST_SIZE: u32 = 256;

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
        let board = RhythmFrontPanelBoard::configure(
            device,
            &options.bitfile_path,
            options.data.enabled_streams,
            options.cable_length_meters,
        )?;

        log::info!("RHD backend ready");
        Ok(Self {
            board,
            config: options.data,
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
        let block =
            parse_rhythm_data_block(packet_id, &raw, &self.config).map_err(RhdReadError::Parse)?;

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

struct RhythmFrontPanelBoard {
    device: FrontPanelDevice,
    board_id: u32,
    board_version: u32,
}

impl RhythmFrontPanelBoard {
    fn configure(
        device: FrontPanelDevice,
        bitfile_path: &Path,
        enabled_streams: usize,
        cable_length_meters: f64,
    ) -> Result<Self, RhdReadError> {
        device
            .configure_fpga(bitfile_path)
            .map_err(RhdReadError::FrontPanel)?;

        device.update_wire_outs();
        let board_id = device.get_wire_out_value(WIRE_OUT_BOARD_ID);
        let board_version = device.get_wire_out_value(WIRE_OUT_BOARD_VERSION);
        log::info!(
            "board_id={board_id} board_version={board_version} (expected board_id={RHYTHM_BOARD_ID})"
        );
        if board_id != RHYTHM_BOARD_ID {
            log::error!(
                "board_id mismatch: the FPGA is not running the expected Rhythm data \
                 plane — either ConfigureFPGA did not actually program this bitfile, or this \
                 bitfile is not the Rhythm/Keyvast data-plane design"
            );
            return Err(RhdReadError::UnexpectedBoardId {
                expected: RHYTHM_BOARD_ID,
                observed: board_id,
            });
        }

        let board = Self {
            device,
            board_id,
            board_version,
        };
        board.reset_board()?;
        board.set_sample_rate_30khz()?;
        board.set_dsp_settle(false)?;
        board.set_cable_length_meters(0, cable_length_meters)?;
        if enabled_streams > 1 {
            board.set_cable_length_meters(1, cable_length_meters)?;
        }
        board.enable_streams(enabled_streams)?;
        board.set_default_data_sources()?;
        board.clear_ttl_out()?;
        log::info!("data plane configured; initializing RHD chips (ADC calibration)...");
        board.initialize_rhd_chips(enabled_streams)?;
        board.set_max_time_step(u32::MAX)?;
        board.set_continuous_run_mode(true)?;
        board.flush_fifo();
        log::info!("board configured and armed for continuous acquisition");

        Ok(board)
    }

    fn reset_board(&self) -> Result<(), RhdReadError> {
        self.device
            .set_wire_in_value(WIRE_IN_RESET_RUN, 0x01, 0x01)
            .map_err(RhdReadError::FrontPanel)?;
        self.device.update_wire_ins();
        self.device
            .set_wire_in_value(WIRE_IN_RESET_RUN, 0x00, 0x01)
            .map_err(RhdReadError::FrontPanel)?;
        self.device.update_wire_ins();

        self.device
            .set_wire_in_value(
                WIRE_IN_MULTI_USE,
                (USB3_BLOCK_SIZE_BYTES / 4) as u32,
                u32::MAX,
            )
            .map_err(RhdReadError::FrontPanel)?;
        self.device.update_wire_ins();
        self.device
            .activate_trigger_in(TRIG_IN_CONFIG, 9)
            .map_err(RhdReadError::FrontPanel)?;

        self.device
            .set_wire_in_value(WIRE_IN_MULTI_USE, RAM_BURST_SIZE, u32::MAX)
            .map_err(RhdReadError::FrontPanel)?;
        self.device.update_wire_ins();
        self.device
            .activate_trigger_in(TRIG_IN_CONFIG, 10)
            .map_err(RhdReadError::FrontPanel)?;

        Ok(())
    }

    /// Program the FPGA PLL for the given sample rate. Returns `true`
    /// if the rate is supported, `false` otherwise.
    /// PLL M/D pairs from Open Ephys `Rhd2000EvalBoard::setSampleRate()`.
    fn set_sample_rate(&self, sample_rate: f64) -> Result<bool, RhdReadError> {
        let (m, d): (u32, u32) = if (sample_rate - 1000.0).abs() < 1.0 {
            (7, 125)
        } else if (sample_rate - 1250.0).abs() < 1.0 {
            (7, 100)
        } else if (sample_rate - 1500.0).abs() < 1.0 {
            (21, 250)
        } else if (sample_rate - 2000.0).abs() < 1.0 {
            (14, 125)
        } else if (sample_rate - 2500.0).abs() < 1.0 {
            (35, 250)
        } else if (sample_rate - 3000.0).abs() < 1.0 {
            (21, 125)
        } else if (sample_rate - 3333.0).abs() < 1.0 {
            (14, 75)
        } else if (sample_rate - 4000.0).abs() < 1.0 {
            (28, 125)
        } else if (sample_rate - 5000.0).abs() < 1.0 {
            (7, 25)
        } else if (sample_rate - 6250.0).abs() < 1.0 {
            (7, 20)
        } else if (sample_rate - 8000.0).abs() < 1.0 {
            (112, 250)
        } else if (sample_rate - 10000.0).abs() < 1.0 {
            (14, 25)
        } else if (sample_rate - 12500.0).abs() < 1.0 {
            (7, 10)
        } else if (sample_rate - 15000.0).abs() < 1.0 {
            (21, 25)
        } else if (sample_rate - 20000.0).abs() < 1.0 {
            (28, 25)
        } else if (sample_rate - 25000.0).abs() < 1.0 {
            (35, 25)
        } else if (sample_rate - 30000.0).abs() < 1.0 {
            (42, 25)
        } else {
            return Ok(false);
        };

        self.wait_for_dcm_done();
        self.device
            .set_wire_in_value(WIRE_IN_DATA_FREQ_PLL, 256 * m + d, u32::MAX)
            .map_err(RhdReadError::FrontPanel)?;
        self.device.update_wire_ins();
        self.device
            .activate_trigger_in(TRIG_IN_CONFIG, 0)
            .map_err(RhdReadError::FrontPanel)?;
        self.wait_for_data_clock_locked();

        Ok(true)
    }

    fn set_sample_rate_30khz(&self) -> Result<(), RhdReadError> {
        self.set_sample_rate(30000.0)?;
        Ok(())
    }

    fn set_max_time_step(&self, max_time_step: u32) -> Result<(), RhdReadError> {
        let lsb = max_time_step & 0x0000_ffff;
        let msb = (max_time_step & 0xffff_0000) >> 16;
        self.device
            .set_wire_in_value(WIRE_IN_MAX_TIME_STEP_LSB, lsb, u32::MAX)
            .map_err(RhdReadError::FrontPanel)?;
        self.device
            .set_wire_in_value(WIRE_IN_MAX_TIME_STEP_MSB, msb, u32::MAX)
            .map_err(RhdReadError::FrontPanel)?;
        self.device.update_wire_ins();
        Ok(())
    }

    fn set_continuous_run_mode(&self, enabled: bool) -> Result<(), RhdReadError> {
        self.device
            .set_wire_in_value(WIRE_IN_RESET_RUN, if enabled { 0x02 } else { 0x00 }, 0x02)
            .map_err(RhdReadError::FrontPanel)?;
        self.device.update_wire_ins();
        Ok(())
    }

    fn enable_streams(&self, enabled_streams: usize) -> Result<(), RhdReadError> {
        let mask = if enabled_streams == 0 {
            0
        } else {
            (1_u32 << enabled_streams) - 1
        };
        self.device
            .set_wire_in_value(WIRE_IN_DATA_STREAM_EN, mask, u32::MAX)
            .map_err(RhdReadError::FrontPanel)?;
        self.device.update_wire_ins();
        Ok(())
    }

    fn clear_ttl_out(&self) -> Result<(), RhdReadError> {
        self.device
            .set_wire_in_value(WIRE_IN_TTL_OUT, 0, u32::MAX)
            .map_err(RhdReadError::FrontPanel)?;
        self.device.update_wire_ins();
        Ok(())
    }

    /// Map each logical data stream to a physical SPI port data source.
    /// Open Ephys `BoardDataSource` enum: PortA1=0, PortA2=1, PortB1=2,
    /// PortB2=3, PortC1=4, PortC2=5, PortD1=6, PortD2=7.
    /// Stream MUX uses `WireInDataStreamSel1234` (0x12) for streams 0-3 (and
    /// 8-11 in upper 16 bits) and `WireInDataStreamSel5678` (0x13) for
    /// streams 4-7 (and 12-15).
    fn set_data_source(&self, stream: u32, source: u32) -> Result<(), RhdReadError> {
        let (endpoint, bit_shift) = match stream {
            0 => (WIRE_IN_DATA_STREAM_SEL_1234, 0),
            1 => (WIRE_IN_DATA_STREAM_SEL_1234, 4),
            2 => (WIRE_IN_DATA_STREAM_SEL_1234, 8),
            3 => (WIRE_IN_DATA_STREAM_SEL_1234, 12),
            4 => (WIRE_IN_DATA_STREAM_SEL_5678, 0),
            5 => (WIRE_IN_DATA_STREAM_SEL_5678, 4),
            6 => (WIRE_IN_DATA_STREAM_SEL_5678, 8),
            7 => (WIRE_IN_DATA_STREAM_SEL_5678, 12),
            8 => (WIRE_IN_DATA_STREAM_SEL_1234, 16),
            9 => (WIRE_IN_DATA_STREAM_SEL_1234, 20),
            10 => (WIRE_IN_DATA_STREAM_SEL_1234, 24),
            11 => (WIRE_IN_DATA_STREAM_SEL_1234, 28),
            12 => (WIRE_IN_DATA_STREAM_SEL_5678, 16),
            13 => (WIRE_IN_DATA_STREAM_SEL_5678, 20),
            14 => (WIRE_IN_DATA_STREAM_SEL_5678, 24),
            15 => (WIRE_IN_DATA_STREAM_SEL_5678, 28),
            _ => {
                return Err(RhdReadError::InvalidPort {
                    port_index: stream as usize,
                });
            }
        };
        self.device
            .set_wire_in_value(endpoint, source << bit_shift, 0x000f << bit_shift)
            .map_err(RhdReadError::FrontPanel)?;
        self.device.update_wire_ins();
        Ok(())
    }

    /// Mirrors Open Ephys `initialize()`: map streams 0-7 to
    /// PortA1, PortB1, PortC1, PortD1, PortA2, PortB2, PortC2, PortD2.
    /// USB3 boards also map streams 8-15 to the same source cycle.
    fn set_default_data_sources(&self) -> Result<(), RhdReadError> {
        // BoardDataSource: PortA1=0, PortA2=1, PortB1=2, PortB2=3,
        //                  PortC1=4, PortC2=5, PortD1=6, PortD2=7
        let sources: [u32; 8] = [0, 2, 4, 6, 1, 3, 5, 7];
        for (stream, &source) in sources.iter().enumerate() {
            self.set_data_source(stream as u32, source)?;
        }
        // USB3: repeat for streams 8-15
        for (stream, &source) in sources.iter().enumerate() {
            self.set_data_source((stream + 8) as u32, source)?;
        }
        Ok(())
    }

    fn set_dsp_settle(&self, enabled: bool) -> Result<(), RhdReadError> {
        self.device
            .set_wire_in_value(WIRE_IN_RESET_RUN, if enabled { 0x04 } else { 0x00 }, 0x04)
            .map_err(RhdReadError::FrontPanel)?;
        self.device.update_wire_ins();
        Ok(())
    }

    fn initialize_rhd_chips(&self, enabled_streams: usize) -> Result<(), RhdReadError> {
        let mut registers = Rhd2000Registers::open_ephys_default();
        registers.set_dig_out_low();

        let dig_out = registers
            .create_command_list_update_dig_out()
            .map_err(RhdReadError::Command)?;
        self.upload_command_list(&dig_out, AuxCommandSlot::AuxCmd1, 0)?;
        self.select_aux_command_length(AuxCommandSlot::AuxCmd1, 0, dig_out.len() - 1)?;
        self.select_aux_command_bank_all_ports(AuxCommandSlot::AuxCmd1, 0)?;

        let temp_sensor = registers
            .create_command_list_temp_sensor()
            .map_err(RhdReadError::Command)?;
        self.upload_command_list(&temp_sensor, AuxCommandSlot::AuxCmd2, 0)?;
        self.select_aux_command_length(AuxCommandSlot::AuxCmd2, 0, temp_sensor.len() - 1)?;
        self.select_aux_command_bank_all_ports(AuxCommandSlot::AuxCmd2, 0)?;

        registers.enable_dsp(true);
        registers.enable_aux_inputs(true);

        let calibrating = registers
            .create_command_list_register_config(true)
            .map_err(RhdReadError::Command)?;
        self.upload_command_list(&calibrating, AuxCommandSlot::AuxCmd3, 0)?;
        self.select_aux_command_length(AuxCommandSlot::AuxCmd3, 0, calibrating.len() - 1)?;

        let normal = registers
            .create_command_list_register_config(false)
            .map_err(RhdReadError::Command)?;
        self.upload_command_list(&normal, AuxCommandSlot::AuxCmd3, 1)?;

        registers.set_fast_settle(true);
        let fast_settle = registers
            .create_command_list_register_config(false)
            .map_err(RhdReadError::Command)?;
        self.upload_command_list(&fast_settle, AuxCommandSlot::AuxCmd3, 2)?;
        registers.set_fast_settle(false);

        self.select_aux_command_bank_all_ports(AuxCommandSlot::AuxCmd3, 0)?;

        // Locate the headstage across ALL SPI ports, not just Port A. Mirrors Open
        // Ephys' DeviceThread::scanPorts(): the USB3 Rhythm design maps 32 data
        // streams across 8 ports (4 streams/port, so port = stream / 4). A headstage
        // plugged into Port B..H therefore lives on streams we never read while only
        // streams 0,1 (Port A's first connector) are enabled — every MISO delay then
        // samples an idle-high line and returns railed 0xFFFF. Probe each port's
        // primary stream pair over all 16 delays and enable whichever port actually
        // has a responding chip. AuxCmd3 bank 0 (register config + ADC calibrate) is
        // selected, so each probe run also configures/calibrates the chip found.
        log::info!("scanning all 8 SPI ports x MISO delays 0..15 to locate the headstage...");
        let (stream_mask, delay) = self.scan_ports_for_headstage(enabled_streams)?;
        self.enable_stream_mask(stream_mask)?;
        self.set_cable_delay_all_ports(delay)?;

        // Final ADC calibration at the chosen delay, then flush the calibration run.
        self.set_max_time_step(RHD_ADC_CALIBRATION_SAMPLES as u32)?;
        self.set_continuous_run_mode(false)?;
        self.run()?;
        self.wait_until_not_running()?;
        self.read_and_discard_samples(enabled_streams, RHD_ADC_CALIBRATION_SAMPLES)?;

        self.select_aux_command_bank_all_ports(AuxCommandSlot::AuxCmd3, 1)?;
        Ok(())
    }

    fn upload_command_list(
        &self,
        commands: &[u16],
        slot: AuxCommandSlot,
        bank: u8,
    ) -> Result<(), RhdReadError> {
        for (address, &command) in commands.iter().enumerate() {
            self.device
                .set_wire_in_value(WIRE_IN_CMD_RAM_DATA, command as u32, u32::MAX)
                .map_err(RhdReadError::FrontPanel)?;
            self.device
                .set_wire_in_value(WIRE_IN_CMD_RAM_ADDR, address as u32, u32::MAX)
                .map_err(RhdReadError::FrontPanel)?;
            self.device
                .set_wire_in_value(WIRE_IN_CMD_RAM_BANK, bank as u32, u32::MAX)
                .map_err(RhdReadError::FrontPanel)?;
            self.device.update_wire_ins();
            self.device
                .activate_trigger_in(TRIG_IN_CONFIG, aux_command_trigger_bit(slot))
                .map_err(RhdReadError::FrontPanel)?;
        }
        Ok(())
    }

    fn select_aux_command_bank_all_ports(
        &self,
        slot: AuxCommandSlot,
        bank: u8,
    ) -> Result<(), RhdReadError> {
        for port in BoardPort::all() {
            self.select_aux_command_bank(port, slot, bank)?;
        }
        Ok(())
    }

    fn select_aux_command_bank(
        &self,
        port: BoardPort,
        slot: AuxCommandSlot,
        bank: u8,
    ) -> Result<(), RhdReadError> {
        let bit_shift = port.bit_shift();
        let endpoint = match slot {
            AuxCommandSlot::AuxCmd1 => WIRE_IN_AUX_CMD_BANK1,
            AuxCommandSlot::AuxCmd2 => WIRE_IN_AUX_CMD_BANK2,
            AuxCommandSlot::AuxCmd3 => WIRE_IN_AUX_CMD_BANK3,
        };

        self.device
            .set_wire_in_value(
                endpoint,
                (bank as u32) << bit_shift,
                0x0000_000f << bit_shift,
            )
            .map_err(RhdReadError::FrontPanel)?;
        self.device.update_wire_ins();
        Ok(())
    }

    fn select_aux_command_length(
        &self,
        slot: AuxCommandSlot,
        loop_index: usize,
        end_index: usize,
    ) -> Result<(), RhdReadError> {
        let bit_shift = match slot {
            AuxCommandSlot::AuxCmd1 => 0,
            AuxCommandSlot::AuxCmd2 => 10,
            AuxCommandSlot::AuxCmd3 => 20,
        };

        self.device
            .set_wire_in_value(
                WIRE_IN_AUX_CMD_LOOP,
                (loop_index as u32) << bit_shift,
                0x0000_03ff << bit_shift,
            )
            .map_err(RhdReadError::FrontPanel)?;
        self.device
            .set_wire_in_value(
                WIRE_IN_AUX_CMD_LENGTH,
                (end_index as u32) << bit_shift,
                0x0000_03ff << bit_shift,
            )
            .map_err(RhdReadError::FrontPanel)?;
        self.device.update_wire_ins();
        Ok(())
    }

    fn set_cable_length_meters(
        &self,
        port_index: usize,
        length_meters: f64,
    ) -> Result<(), RhdReadError> {
        let speed_of_light = 299_792_458.0_f64;
        let xilinx_lvds_output_delay = 1.9e-9_f64;
        let xilinx_lvds_input_delay = 1.4e-9_f64;
        let rhd2000_delay = 9.0e-9_f64;
        let miso_settle_time = 6.7e-9_f64;

        let t_step = 1.0 / (2800.0 * DEFAULT_RHD_SAMPLE_RATE);
        let cable_velocity = 0.555 * speed_of_light;
        let distance = 2.0 * length_meters;
        let time_delay = distance / cable_velocity
            + xilinx_lvds_output_delay
            + rhd2000_delay
            + xilinx_lvds_input_delay
            + miso_settle_time;
        let delay = ((time_delay / t_step) + 1.0).round().max(1.0) as u32;
        let bit_shift = port_index
            .checked_mul(4)
            .ok_or(RhdReadError::InvalidPort { port_index })?;
        if bit_shift > 28 {
            return Err(RhdReadError::InvalidPort { port_index });
        }

        self.device
            .set_wire_in_value(WIRE_IN_MISO_DELAY, delay << bit_shift, 0x0f << bit_shift)
            .map_err(RhdReadError::FrontPanel)?;
        self.device.update_wire_ins();
        Ok(())
    }

    fn run(&self) -> Result<(), RhdReadError> {
        self.device
            .activate_trigger_in(TRIG_IN_SPI_START, 0)
            .map_err(RhdReadError::FrontPanel)
    }

    fn start_continuous_acquisition(&self) -> Result<(), RhdReadError> {
        self.set_max_time_step(u32::MAX)?;
        self.set_continuous_run_mode(true)?;
        self.run()
    }

    fn read_raw_block(&self, config: &RhythmDataConfig) -> Result<Vec<u8>, RhdReadError> {
        let block_bytes = bytes_per_block(config.enabled_streams, config.samples_per_block)
            .map_err(RhdReadError::InvalidConfig)?;
        let needed_words = (block_bytes / 2) as u32;

        self.wait_for_fifo_words(needed_words)?;

        let mut buffer = vec![0_u8; block_bytes];
        let read = self
            .device
            .read_from_block_pipe_out(PIPE_OUT_DATA, USB3_BLOCK_SIZE_BYTES, &mut buffer)
            .map_err(RhdReadError::FrontPanel)?;
        if read != block_bytes {
            return Err(RhdReadError::ShortPipeRead {
                expected: block_bytes,
                observed: read,
            });
        }

        Ok(buffer)
    }

    fn read_and_discard_samples(
        &self,
        enabled_streams: usize,
        samples: usize,
    ) -> Result<(), RhdReadError> {
        self.read_pipe_block(enabled_streams, samples)?;
        Ok(())
    }

    fn read_pipe_block(
        &self,
        enabled_streams: usize,
        samples: usize,
    ) -> Result<Vec<u8>, RhdReadError> {
        let byte_count =
            bytes_per_block(enabled_streams, samples).map_err(RhdReadError::InvalidConfig)?;
        self.wait_for_fifo_words((byte_count / 2) as u32)?;
        let mut buffer = vec![0_u8; byte_count];
        let read = self
            .device
            .read_from_block_pipe_out(PIPE_OUT_DATA, USB3_BLOCK_SIZE_BYTES, &mut buffer)
            .map_err(RhdReadError::FrontPanel)?;
        if read != byte_count {
            return Err(RhdReadError::ShortPipeRead {
                expected: byte_count,
                observed: read,
            });
        }
        Ok(buffer)
    }

    fn set_cable_delay_all_ports(&self, delay: u32) -> Result<(), RhdReadError> {
        let delay = delay.min(15);
        let mut value = 0_u32;
        for port in 0..8 {
            value |= delay << (port * 4);
        }
        self.device
            .set_wire_in_value(WIRE_IN_MISO_DELAY, value, 0xffff_ffff)
            .map_err(RhdReadError::FrontPanel)?;
        self.device.update_wire_ins();
        Ok(())
    }

    /// Set MISO delay for a single port (0=A … 7=H) without affecting others.
    fn set_cable_delay_port(&self, port: usize, delay: u32) -> Result<(), RhdReadError> {
        let delay = delay.min(15);
        let shift = (port as u32) * 4;
        self.device
            .set_wire_in_value(WIRE_IN_MISO_DELAY, delay << shift, 0x0f << shift)
            .map_err(RhdReadError::FrontPanel)?;
        self.device.update_wire_ins();
        Ok(())
    }

    /// Write an arbitrary data-stream enable mask to the FPGA. Bit `i` enables
    /// physical data stream `i` (stream `i` belongs to SPI port `i / 4`).
    fn enable_stream_mask(&self, mask: u32) -> Result<(), RhdReadError> {
        self.device
            .set_wire_in_value(WIRE_IN_DATA_STREAM_EN, mask, u32::MAX)
            .map_err(RhdReadError::FrontPanel)?;
        self.device.update_wire_ins();
        Ok(())
    }

    /// Probe every SPI port (A..H) for a responding RHD headstage and return the
    /// `(stream-enable mask, MISO cable delay)` to use for acquisition. Mirrors Open
    /// Ephys' DeviceThread::scanPorts(): for each port we enable that port's primary
    /// stream pair (streams `4*port` and `4*port + 1` = the headstage's MISO A/B) and
    /// sweep all 16 FPGA MISO delays, measuring how many amplifier words are railed
    /// (idle-high 0xFFFF / 0x0000). A correctly-delayed, populated port reports ~0%
    /// railed; an empty port reports ~100% at every delay. We keep the least-railed
    /// port and, like scanPorts, pick a middle "good" delay for timing margin.
    ///
    /// Each probe enables exactly `enabled_streams` streams (the same count
    /// acquisition will use), so the FPGA frame size during the scan matches what the
    /// parser expects — only the *physical* port behind those stream slots changes.
    /// Falls back to Port A if nothing responds. AuxCmd3 bank 0 (register config + ADC
    /// calibrate) must be selected so each run also configures/calibrates the chip.
    fn scan_ports_for_headstage(&self, enabled_streams: usize) -> Result<(u32, u32), RhdReadError> {
        const PROBE_SAMPLES: usize = 128;
        const PORT_LETTERS: [char; 8] = ['A', 'B', 'C', 'D', 'E', 'F', 'G', 'H'];

        let stream_bits = (1_u32 << enabled_streams) - 1;

        // (port index, chosen delay, has_chip_id)
        let mut best: Option<(usize, u32, bool)> = None;

        for (port, &port_letter) in PORT_LETTERS.iter().enumerate() {
            let first_stream = (port * 4) as u32;
            self.enable_stream_mask(stream_bits << first_stream)?;

            // Delays where chip ID was verified (strong validation).
            let mut id_verified_delays: Vec<u32> = Vec::new();
            // Delays where railed fraction < 50% (weak fallback).
            let mut low_railed_delays: Vec<u32> = Vec::new();

            for delay in 0..16_u32 {
                self.set_cable_delay_all_ports(delay)?;
                self.set_max_time_step(PROBE_SAMPLES as u32)?;
                self.set_continuous_run_mode(false)?;
                self.run()?;
                self.wait_until_not_running()?;
                let raw = self.read_pipe_block(enabled_streams, PROBE_SAMPLES)?;

                let has_id = verify_chip_id_in_probe(&raw, enabled_streams, PROBE_SAMPLES);
                if has_id {
                    id_verified_delays.push(delay);
                }

                let railed = min_stream_railed_fraction(&raw, enabled_streams, PROBE_SAMPLES);
                if railed < 0.5 {
                    low_railed_delays.push(delay);
                }
            }

            // Prefer chip-ID-verified delays; fall back to low-railed delays.
            let (good_delays, validated_by_id) = if !id_verified_delays.is_empty() {
                (id_verified_delays, true)
            } else {
                (low_railed_delays, false)
            };

            // Mirror scanPorts: 1-2 good delays -> first; >2 -> a middle one (margin).
            let chosen_delay = if good_delays.len() > 2 {
                good_delays[good_delays.len() / 2]
            } else if let Some(&d) = good_delays.first() {
                d
            } else {
                continue;
            };

            let method = if validated_by_id {
                "chip ID"
            } else {
                "railed fraction"
            };
            log::info!(
                "port {} ({}): {} good delays ({} verified) @ chosen delay {}  <- responding",
                port_letter,
                stream_range_label(first_stream, enabled_streams),
                good_delays.len(),
                method,
                chosen_delay,
            );

            // Prefer chip-ID-verified ports over railed-fraction-only ports.
            let dominated = best
                .as_ref()
                .is_some_and(|&(_, _, prev_id)| prev_id && !validated_by_id);
            if !dominated
                && best
                    .as_ref()
                    .is_none_or(|&(_, _, prev_id)| validated_by_id >= prev_id)
            {
                best = Some((port, chosen_delay, validated_by_id));
            }
        }

        match best {
            Some((port, delay, _)) => {
                let first_stream = (port * 4) as u32;
                log::info!(
                    "FOUND headstage on port {} ({}) at MISO delay {}",
                    PORT_LETTERS[port],
                    stream_range_label(first_stream, enabled_streams),
                    delay,
                );
                // Apply per-port delay only for the discovered port.
                self.set_cable_delay_port(port, delay)?;
                Ok((stream_bits << first_stream, delay))
            }
            None => {
                log::warn!(
                    "no responding RHD chip found on any of the 8 SPI ports. \
                     Defaulting to Port A at delay 0; expect flat data. Check that the headstage \
                     is connected and powered, and that this is a KeyVast bitstream (the stock \
                     Intan bit cannot drive the KeyVast headstage SPI pins)."
                );
                Ok((stream_bits, 0))
            }
        }
    }

    fn wait_for_fifo_words(&self, needed_words: u32) -> Result<(), RhdReadError> {
        for _ in 0..200 {
            let available = self.num_words_in_fifo();
            if available >= needed_words {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(5));
        }

        Err(RhdReadError::NotEnoughFifoWords {
            needed: needed_words,
            available: self.num_words_in_fifo(),
        })
    }

    fn flush_fifo(&self) {
        // Set USB3 pipeout block-throttle override (bit 16 of WireInResetRun)
        // so the FPGA allows reads of any size during flush.
        let _ = self
            .device
            .set_wire_in_value(WIRE_IN_RESET_RUN, 1 << 16, 1 << 16);
        self.device.update_wire_ins();

        // Phase A: bulk drain with large reads (up to 256 KB per iteration).
        const FLUSH_CHUNK: usize = 256 * 1024;
        for _ in 0..10_000 {
            let available_words = self.num_words_in_fifo();
            if (available_words as usize) < FLUSH_CHUNK / 2 {
                break;
            }
            let mut buffer = vec![0_u8; FLUSH_CHUNK];
            let _ = self.device.read_from_block_pipe_out(
                PIPE_OUT_DATA,
                USB3_BLOCK_SIZE_BYTES,
                &mut buffer,
            );
        }

        // Phase B: drain remaining with appropriately-sized reads.
        for _ in 0..10_000 {
            let available_words = self.num_words_in_fifo();
            if available_words == 0 {
                break;
            }
            let byte_count = (available_words as usize).saturating_mul(2);
            // Round up to USB3_BLOCK_SIZE_BYTES boundary.
            let aligned = byte_count.div_ceil(USB3_BLOCK_SIZE_BYTES).max(1) * USB3_BLOCK_SIZE_BYTES;
            let mut buffer = vec![0_u8; aligned];
            let _ = self.device.read_from_block_pipe_out(
                PIPE_OUT_DATA,
                USB3_BLOCK_SIZE_BYTES,
                &mut buffer,
            );
        }

        // Release throttle override.
        let _ = self.device.set_wire_in_value(WIRE_IN_RESET_RUN, 0, 1 << 16);
        self.device.update_wire_ins();

        if self.num_words_in_fifo() > 0 {
            log::warn!("flush_fifo did not fully drain");
        }
    }

    fn num_words_in_fifo(&self) -> u32 {
        self.device.update_wire_outs();
        let msb = self.device.get_wire_out_value(WIRE_OUT_NUM_WORDS_MSB);
        let lsb = self.device.get_wire_out_value(WIRE_OUT_NUM_WORDS_LSB);
        (msb << 16) | (lsb & 0xFFFF)
    }

    fn wait_for_dcm_done(&self) {
        for _ in 0..100 {
            self.device.update_wire_outs();
            if self.device.get_wire_out_value(WIRE_OUT_DATA_CLK_LOCKED) & 0x02 != 0 {
                return;
            }
            thread::sleep(Duration::from_millis(1));
        }
    }

    fn wait_for_data_clock_locked(&self) {
        for _ in 0..100 {
            self.device.update_wire_outs();
            if self.device.get_wire_out_value(WIRE_OUT_DATA_CLK_LOCKED) & 0x01 != 0 {
                return;
            }
            thread::sleep(Duration::from_millis(1));
        }
    }

    fn wait_until_not_running(&self) -> Result<(), RhdReadError> {
        for _ in 0..200 {
            if !self.is_running() {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(5));
        }

        Err(RhdReadError::SpiStillRunning)
    }

    #[allow(dead_code)]
    fn is_running(&self) -> bool {
        self.device.update_wire_outs();
        self.device.get_wire_out_value(WIRE_OUT_SPI_RUNNING) & 0x01 != 0
    }

    #[allow(dead_code)]
    fn board_id(&self) -> u32 {
        self.board_id
    }

    #[allow(dead_code)]
    fn board_version(&self) -> u32 {
        self.board_version
    }

    // ------------------------------------------------------------------
    // Stub methods for features that Open Ephys supports but keyvast has
    // not yet wired up.  They are marked `#[allow(dead_code)]` until the
    // GUI or a command interface calls them.
    // ------------------------------------------------------------------

    /// Set the on-chip DAC for impedance testing waveform generation.
    #[allow(dead_code)]
    fn set_dac_threshold(&self, _dac_channel: u8, _threshold: u16) -> Result<(), RhdReadError> {
        // TODO: WireInDacSource1..8 + WireInDacManual + TrigIn bits.
        Ok(())
    }

    /// Enable/disable an on-board LED.
    #[allow(dead_code)]
    fn set_led(&self, _led_index: u8, _on: bool) -> Result<(), RhdReadError> {
        // TODO: WireInLedDisplay register.
        Ok(())
    }

    /// Trigger external fast-settle (blanking) via the FPGA logic line.
    #[allow(dead_code)]
    fn set_external_fast_settle_channel(&self, _channel: u8) -> Result<(), RhdReadError> {
        // TODO: WireInExternalFastSettle.
        Ok(())
    }

    /// Run impedance measurement across all channels using the on-chip DAC.
    ///
    /// Algorithm (port of Intan RHX `impedancereader.cpp`):
    /// 1. Upload DC waveform to AuxCmd1 Bank 0, sine wave to AuxCmd1 Bank 1.
    /// 2. Upload register configs with zcheck enabled + 3 cap scales to
    ///    AuxCmd3 Banks 2/3/4.
    /// 3. For each channel: set zcheck_select, switch banks, run acquisition.
    /// 4. Compute impedance magnitude/phase via DFT at the test frequency.
    /// 5. Auto-select the best capacitor scale and re-measure if needed.
    pub fn run_impedance_test(
        &self,
        config: &impedance::ImpedanceTestConfig,
        enabled_streams: usize,
        progress_callback: Option<&dyn Fn(usize, usize)>,
    ) -> Result<impedance::ImpedanceResult, RhdReadError> {
        use crate::commands::ZcheckScale;

        log::info!(
            "starting impedance test: freq={:.0} Hz, {} channels, {} periods",
            config.frequency_hz,
            config.channel_count,
            config.num_periods
        );

        let mut registers = Rhd2000Registers::open_ephys_default();

        // ── Step 1: Upload DAC waveforms to AuxCmd1 ──────────────
        // Bank 0: DC (flat mid-scale).
        let dc_dac = registers
            .create_command_list_zcheck_dac(0.0, 0.0)
            .map_err(RhdReadError::Command)?;
        self.upload_command_list(&dc_dac, AuxCommandSlot::AuxCmd1, 0)?;
        self.select_aux_command_length(AuxCommandSlot::AuxCmd1, 0, dc_dac.len() - 1)?;

        // Bank 1: sine wave at the test frequency.
        let sine_dac = registers
            .create_command_list_zcheck_dac(config.frequency_hz, config.dac_amplitude)
            .map_err(RhdReadError::Command)?;
        self.upload_command_list(&sine_dac, AuxCommandSlot::AuxCmd1, 1)?;

        // ── Step 2: Upload zcheck register configs to AuxCmd3 ────
        registers.enable_zcheck(true);

        registers.set_zcheck_scale(ZcheckScale::Cs100fF);
        registers.set_zcheck_polarity(false);
        let zcheck_100ff = registers
            .create_command_list_register_config(false)
            .map_err(RhdReadError::Command)?;
        self.upload_command_list(&zcheck_100ff, AuxCommandSlot::AuxCmd3, 2)?;

        registers.set_zcheck_scale(ZcheckScale::Cs1pF);
        let zcheck_1pf = registers
            .create_command_list_register_config(false)
            .map_err(RhdReadError::Command)?;
        self.upload_command_list(&zcheck_1pf, AuxCommandSlot::AuxCmd3, 3)?;

        registers.set_zcheck_scale(ZcheckScale::Cs10pF);
        let zcheck_10pf = registers
            .create_command_list_register_config(false)
            .map_err(RhdReadError::Command)?;
        self.upload_command_list(&zcheck_10pf, AuxCommandSlot::AuxCmd3, 4)?;

        // ── Step 3: Measure each channel ─────────────────────────
        let samples_needed = config.total_samples();
        let channels = config
            .channel_count
            .min(CHANNELS_PER_STREAM * enabled_streams);

        let mut results: Vec<impedance::ChannelImpedance> = Vec::with_capacity(channels);

        // Start with sine wave on AuxCmd1 Bank 1.
        self.select_aux_command_bank_all_ports(AuxCommandSlot::AuxCmd1, 1)?;
        self.select_aux_command_length(AuxCommandSlot::AuxCmd1, 0, sine_dac.len() - 1)?;

        for ch in 0..channels {
            if let Some(cb) = progress_callback {
                cb(ch, channels);
            }

            let chip_channel = (ch % CHANNELS_PER_STREAM) as u8;
            registers.set_zcheck_channel(chip_channel);

            // Initial measurement with 1 pF (Bank 3).
            registers.set_zcheck_scale(ZcheckScale::Cs1pF);
            let updated_cfg = registers
                .create_command_list_register_config(false)
                .map_err(RhdReadError::Command)?;
            self.upload_command_list(&updated_cfg, AuxCommandSlot::AuxCmd3, 3)?;
            self.select_aux_command_bank_all_ports(AuxCommandSlot::AuxCmd3, 3)?;

            self.flush_fifo();
            self.set_max_time_step(samples_needed as u32)?;
            self.set_continuous_run_mode(false)?;
            self.run()?;
            self.wait_until_not_running()?;

            let raw = self.read_pipe_block(enabled_streams, samples_needed)?;
            let amp_data = extract_channel_from_raw(&raw, enabled_streams, samples_needed, ch);

            let (mag_1pf, phase_1pf) = impedance::compute_impedance(
                &amp_data,
                config.sample_rate,
                config.frequency_hz,
                ZcheckScale::Cs1pF,
            );

            // Auto-select the best scale and re-measure if needed.
            let best_scale = impedance::auto_select_scale(mag_1pf);

            let (magnitude, phase, scale) = if best_scale != ZcheckScale::Cs1pF {
                registers.set_zcheck_scale(best_scale);
                let re_cfg = registers
                    .create_command_list_register_config(false)
                    .map_err(RhdReadError::Command)?;

                let bank = match best_scale {
                    ZcheckScale::Cs100fF => 2,
                    ZcheckScale::Cs1pF => 3,
                    ZcheckScale::Cs10pF => 4,
                };

                self.upload_command_list(&re_cfg, AuxCommandSlot::AuxCmd3, bank)?;
                self.select_aux_command_bank_all_ports(AuxCommandSlot::AuxCmd3, bank)?;

                self.flush_fifo();
                self.set_max_time_step(samples_needed as u32)?;
                self.set_continuous_run_mode(false)?;
                self.run()?;
                self.wait_until_not_running()?;

                let raw2 = self.read_pipe_block(enabled_streams, samples_needed)?;
                let amp2 = extract_channel_from_raw(&raw2, enabled_streams, samples_needed, ch);

                let (mag, ph) = impedance::compute_impedance(
                    &amp2,
                    config.sample_rate,
                    config.frequency_hz,
                    best_scale,
                );
                (mag, ph, best_scale)
            } else {
                (mag_1pf, phase_1pf, ZcheckScale::Cs1pF)
            };

            results.push(impedance::ChannelImpedance {
                channel: ch,
                magnitude_ohms: magnitude,
                phase_degrees: phase,
                scale_used: scale,
                valid: magnitude.is_finite() && magnitude > 0.0,
            });
        }

        // ── Step 4: Restore normal operation ─────────────────────
        self.select_aux_command_bank_all_ports(AuxCommandSlot::AuxCmd1, 0)?;
        self.select_aux_command_length(AuxCommandSlot::AuxCmd1, 0, dc_dac.len() - 1)?;

        registers.enable_zcheck(false);
        let normal_cfg = registers
            .create_command_list_register_config(false)
            .map_err(RhdReadError::Command)?;
        self.upload_command_list(&normal_cfg, AuxCommandSlot::AuxCmd3, 1)?;
        self.select_aux_command_bank_all_ports(AuxCommandSlot::AuxCmd3, 1)?;

        registers.set_dig_out_low();
        let dig_out = registers
            .create_command_list_update_dig_out()
            .map_err(RhdReadError::Command)?;
        self.upload_command_list(&dig_out, AuxCommandSlot::AuxCmd1, 0)?;
        self.select_aux_command_length(AuxCommandSlot::AuxCmd1, 0, dig_out.len() - 1)?;
        self.select_aux_command_bank_all_ports(AuxCommandSlot::AuxCmd1, 0)?;

        self.flush_fifo();

        log::info!(
            "impedance test complete: {} channels measured",
            results.len()
        );

        Ok(impedance::ImpedanceResult {
            config: config.clone(),
            channels: results,
        })
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
            | Self::SpiStillRunning => None,
        }
    }
}

fn aux_command_trigger_bit(slot: AuxCommandSlot) -> i32 {
    match slot {
        AuxCommandSlot::AuxCmd1 => 1,
        AuxCommandSlot::AuxCmd2 => 2,
        AuxCommandSlot::AuxCmd3 => 3,
    }
}

/// Compact label for the contiguous data-stream range a port probe enables,
/// e.g. "stream 4" (one 32-channel stream) or "streams 4-5" (64-channel pair).
fn stream_range_label(first_stream: u32, enabled_streams: usize) -> String {
    if enabled_streams <= 1 {
        format!("stream {first_stream}")
    } else {
        format!(
            "streams {first_stream}-{}",
            first_stream + enabled_streams as u32 - 1
        )
    }
}

/// Verify that the AuxCmd3 results in a probe block contain the RHD chip
/// identification signature "INTAN" (registers 63, 62, 61, 60, 59). This is a
/// strong validation — it proves SPI communication is correctly aligned and a
/// real RHD2000 chip is responding. Returns true if the pattern is found on any
/// stream.
///
/// The register config command list (`create_command_list_register_config`) reads
/// these identification registers after writing config registers. The MISO
/// response for each command arrives one SPI cycle later, so the 'I','N','T','A','N'
/// bytes appear in consecutive AuxCmd3 result words. Rather than relying on exact
/// command-list indices, we scan all samples for the pattern.
fn verify_chip_id_in_probe(raw: &[u8], enabled_streams: usize, samples: usize) -> bool {
    if enabled_streams == 0 || samples < 5 {
        return false;
    }

    // Frame layout per sample (in bytes):
    //   8 (magic) + 4 (timestamp) + 3*enabled_streams*2 (aux results)
    //   + CHANNELS_PER_STREAM*enabled_streams*2 (amplifier)
    //   + (enabled_streams%4)*2 (pad) + 8*2 (board ADC) + 2 (TTL in) + 2 (TTL out)
    let frame_bytes =
        (4 + 2 + enabled_streams * (CHANNELS_PER_STREAM + 3) + (enabled_streams % 4) + 8 + 2) * 2;

    // AuxCmd3 results start after magic(8) + timestamp(4) + AuxCmd1(streams*2) + AuxCmd2(streams*2)
    let auxcmd3_base = 12 + 2 * enabled_streams * 2;

    let pattern: [u8; 5] = [b'I', b'N', b'T', b'A', b'N'];

    for stream in 0..enabled_streams {
        let word_offset_in_frame = auxcmd3_base + stream * 2;
        let mut aux_bytes: Vec<u8> = Vec::with_capacity(samples);
        for s in 0..samples {
            let off = s * frame_bytes + word_offset_in_frame;
            if off + 2 > raw.len() {
                return false;
            }
            let word = u16::from_le_bytes([raw[off], raw[off + 1]]);
            aux_bytes.push((word & 0xFF) as u8);
        }
        if aux_bytes.windows(5).any(|w| w == pattern) {
            return true;
        }
    }
    false
}

/// Fraction (0.0..=1.0) of amplifier words that are railed/invalid (0xFFFF or
/// 0x0000) on the best-communicating stream, evaluated over the second half of
/// the block (after the in-run register config + ADC calibration has settled).
/// A correctly-delayed, responding chip reports ~0%; a wrong delay samples the
/// idle MISO line and reports ~100%. The byte walk mirrors `parse_rhythm_data_block`.
fn min_stream_railed_fraction(raw: &[u8], enabled_streams: usize, samples: usize) -> f64 {
    if enabled_streams == 0 || samples == 0 {
        return 1.0;
    }
    let mut railed = vec![0_usize; enabled_streams];
    let mut total = vec![0_usize; enabled_streams];
    let evaluate_from = samples / 2;
    let mut offset = 0_usize;

    for sample_index in 0..samples {
        offset += 8; // frame magic
        offset += 4; // timestamp
        offset += 3 * enabled_streams * 2; // aux command results
        for _channel in 0..CHANNELS_PER_STREAM {
            for stream in 0..enabled_streams {
                if offset + 2 > raw.len() {
                    return 1.0;
                }
                let word = u16::from_le_bytes([raw[offset], raw[offset + 1]]);
                offset += 2;
                if sample_index >= evaluate_from {
                    if word == 0xffff || word == 0x0000 {
                        railed[stream] += 1;
                    }
                    total[stream] += 1;
                }
            }
        }
        offset += (enabled_streams % 4) * 2; // alignment padding
        offset += 8 * 2; // auxiliary ADC slots
        offset += 2; // TTL in
        offset += 2; // TTL out
    }

    (0..enabled_streams)
        .map(|stream| {
            if total[stream] == 0 {
                1.0
            } else {
                railed[stream] as f64 / total[stream] as f64
            }
        })
        .fold(f64::INFINITY, f64::min)
}

/// Extract raw i16 amplifier samples for a single logical channel from a raw
/// data block.  Channel `ch` maps to stream `ch / CHANNELS_PER_STREAM`, intra-
/// stream channel `ch % CHANNELS_PER_STREAM`.  Layout mirrors
/// `parse_rhythm_data_block`.
fn extract_channel_from_raw(
    raw: &[u8],
    enabled_streams: usize,
    samples: usize,
    ch: usize,
) -> Vec<i16> {
    use crate::protocol::raw_word_to_signed_count;

    let stream = ch / CHANNELS_PER_STREAM;
    let intra_ch = ch % CHANNELS_PER_STREAM;

    // Frame layout (in u16 words): 4 (magic) + 2 (timestamp)
    // + 3*enabled_streams (aux) + CHANNELS_PER_STREAM*enabled_streams (amp)
    // + (enabled_streams%4) (pad) + 8 (board ADC) + 1 (TTL in) + 1 (TTL out)
    let frame_words =
        4 + 2 + enabled_streams * (CHANNELS_PER_STREAM + 3) + (enabled_streams % 4) + 8 + 2;
    let frame_bytes = frame_words * 2;

    // Amplifier data starts after magic(4w) + timestamp(2w) + aux(3*streams w).
    let amp_base_words = 4 + 2 + 3 * enabled_streams;

    let mut out = Vec::with_capacity(samples);
    for s in 0..samples {
        // Within the amplifier section: data is channel-major, stream-minor.
        // Word index = amp_base + (intra_ch * enabled_streams + stream)
        let word_idx = amp_base_words + intra_ch * enabled_streams + stream;
        let byte_off = s * frame_bytes + word_idx * 2;
        if byte_off + 2 > raw.len() {
            break;
        }
        let word = u16::from_le_bytes([raw[byte_off], raw[byte_off + 1]]);
        out.push(raw_word_to_signed_count(word));
    }
    out
}
