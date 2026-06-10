use std::{fmt, path::PathBuf, thread, time::Duration};

use kv_types::SampleBlock;

use crate::{
    commands::{
        AuxCommandSlot, BoardPort, RHD_ADC_CALIBRATION_SAMPLES, Rhd2000Registers, RhdCommandError,
    },
    frontpanel::{FrontPanelDevice, FrontPanelError, FrontPanelLibrary},
    parser::{RhythmParseError, parse_rhythm_data_block},
    protocol::{
        CHANNELS_PER_STREAM, DEFAULT_RHD_SAMPLE_RATE, RHYTHM_BOARD_ID, RhythmConfigError,
        RhythmDataConfig, SAMPLES_PER_USB_BLOCK, USB3_BLOCK_SIZE_BYTES, bytes_per_block,
    },
};

const WIRE_IN_RESET_RUN: i32 = 0x00;
const WIRE_IN_MAX_TIME_STEP: i32 = 0x01;
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
const WIRE_IN_DATA_STREAM_EN: i32 = 0x14;
const WIRE_IN_TTL_OUT: i32 = 0x15;
const WIRE_IN_MULTI_USE: i32 = 0x1f;
const WIRE_OUT_NUM_WORDS: i32 = 0x20;
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
                device_id: "rhd-xem7310".to_string(),
                stream_id: 0,
                enabled_streams,
                sample_rate: DEFAULT_RHD_SAMPLE_RATE,
                samples_per_block: SAMPLES_PER_USB_BLOCK,
            },
            cable_length_meters: 0.9144,
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
        eprintln!(
            "[kv-rhd] opening RHD backend: bitfile={}, streams={}",
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

        eprintln!("[kv-rhd] RHD backend ready");
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
            eprintln!("[kv-rhd] continuous acquisition started; reading first block...");
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
            eprintln!(
                "[kv-rhd] first block OK: {} channels x {} samples, raw amplifier i16 min={min} max={max}{note}",
                block.channel_count, block.samples_per_channel
            );
        }

        Ok(block)
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
        bitfile_path: &PathBuf,
        enabled_streams: usize,
        cable_length_meters: f64,
    ) -> Result<Self, RhdReadError> {
        device
            .configure_fpga(bitfile_path)
            .map_err(RhdReadError::FrontPanel)?;

        device.update_wire_outs();
        let board_id = device.get_wire_out_value(WIRE_OUT_BOARD_ID);
        let board_version = device.get_wire_out_value(WIRE_OUT_BOARD_VERSION);
        eprintln!(
            "[kv-rhd] board_id={board_id} board_version={board_version} (expected board_id={RHYTHM_BOARD_ID})"
        );
        if board_id != RHYTHM_BOARD_ID {
            eprintln!(
                "[kv-rhd] board_id mismatch: the FPGA is not running the expected Rhythm data \
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
        board.clear_ttl_out()?;
        eprintln!("[kv-rhd] data plane configured; initializing RHD chips (ADC calibration)...");
        board.initialize_rhd_chips(enabled_streams)?;
        board.set_max_time_step(u32::MAX)?;
        board.set_continuous_run_mode(true)?;
        board.flush_fifo();
        eprintln!("[kv-rhd] board configured and armed for continuous acquisition");

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
            .set_wire_in_value(WIRE_IN_MULTI_USE, (USB3_BLOCK_SIZE_BYTES / 4) as u32, u32::MAX)
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

    fn set_sample_rate_30khz(&self) -> Result<(), RhdReadError> {
        let m = 42_u32;
        let d = 25_u32;

        self.wait_for_dcm_done();
        self.device
            .set_wire_in_value(WIRE_IN_DATA_FREQ_PLL, 256 * m + d, u32::MAX)
            .map_err(RhdReadError::FrontPanel)?;
        self.device.update_wire_ins();
        self.device
            .activate_trigger_in(TRIG_IN_CONFIG, 0)
            .map_err(RhdReadError::FrontPanel)?;
        self.wait_for_data_clock_locked();

        Ok(())
    }

    fn set_max_time_step(&self, max_time_step: u32) -> Result<(), RhdReadError> {
        self.device
            .set_wire_in_value(WIRE_IN_MAX_TIME_STEP, max_time_step, u32::MAX)
            .map_err(RhdReadError::FrontPanel)?;
        self.device.update_wire_ins();
        Ok(())
    }

    fn set_continuous_run_mode(&self, enabled: bool) -> Result<(), RhdReadError> {
        self.device
            .set_wire_in_value(
                WIRE_IN_RESET_RUN,
                if enabled { 0x02 } else { 0x00 },
                0x02,
            )
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

    fn set_dsp_settle(&self, enabled: bool) -> Result<(), RhdReadError> {
        self.device
            .set_wire_in_value(
                WIRE_IN_RESET_RUN,
                if enabled { 0x04 } else { 0x00 },
                0x04,
            )
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
        eprintln!(
            "[kv-rhd] scanning all 8 SPI ports x MISO delays 0..15 to locate the headstage..."
        );
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

        // Probe (and ultimately enable) exactly `enabled_streams` streams per port so
        // the FPGA frame size during the scan matches what acquisition will parse.
        // Enabling a different stream count than the parser expects leaves the data
        // stream misaligned and triggers a "bad Rhythm frame magic" error.
        let stream_bits = (1_u32 << enabled_streams) - 1;

        // (port index, chosen delay, railed fraction at best delay)
        let mut best: Option<(usize, u32, f64)> = None;

        for port in 0..8_usize {
            let first_stream = (port * 4) as u32;
            self.enable_stream_mask(stream_bits << first_stream)?;

            let mut good_delays: Vec<u32> = Vec::new();
            let mut min_railed = f64::INFINITY;
            let mut min_railed_delay = 0_u32;

            for delay in 0..16_u32 {
                self.set_cable_delay_all_ports(delay)?;
                self.set_max_time_step(PROBE_SAMPLES as u32)?;
                self.set_continuous_run_mode(false)?;
                self.run()?;
                self.wait_until_not_running()?;
                let raw = self.read_pipe_block(enabled_streams, PROBE_SAMPLES)?;
                let railed = min_stream_railed_fraction(&raw, enabled_streams, PROBE_SAMPLES);

                if railed < min_railed {
                    min_railed = railed;
                    min_railed_delay = delay;
                }
                if railed < 0.5 {
                    good_delays.push(delay);
                }
            }

            // Mirror scanPorts: 1-2 good delays -> first; >2 -> a middle one (margin).
            let chosen_delay = if good_delays.len() > 2 {
                good_delays[good_delays.len() / 2]
            } else {
                good_delays.first().copied().unwrap_or(min_railed_delay)
            };

            let responding = min_railed < 0.5;
            eprintln!(
                "[kv-rhd] port {} ({}): best invalid words {:>3.0}% @ delay {}{}",
                PORT_LETTERS[port],
                stream_range_label(first_stream, enabled_streams),
                min_railed * 100.0,
                chosen_delay,
                if responding { "  <- chip responding" } else { "" }
            );

            if best.map_or(true, |(_, _, best_railed)| min_railed < best_railed) {
                best = Some((port, chosen_delay, min_railed));
            }
        }

        match best {
            Some((port, delay, railed)) if railed < 0.5 => {
                let first_stream = (port * 4) as u32;
                eprintln!(
                    "[kv-rhd] FOUND headstage on port {} ({}) at MISO delay {} ({:.0}% invalid words)",
                    PORT_LETTERS[port],
                    stream_range_label(first_stream, enabled_streams),
                    delay,
                    railed * 100.0
                );
                Ok((stream_bits << first_stream, delay))
            }
            other => {
                let delay = other.map(|(_, delay, _)| delay).unwrap_or(0);
                let railed = other.map(|(_, _, railed)| railed).unwrap_or(1.0);
                eprintln!(
                    "[kv-rhd] WARNING: no responding RHD chip found on any of the 8 SPI ports \
                     (best port was still {:.0}% invalid). Defaulting to Port A at delay {delay}; \
                     expect flat data. Check that the headstage is connected and powered, and that \
                     this is a KeyVast bitstream (the stock Intan bit cannot drive the KeyVast \
                     headstage SPI pins).",
                    railed * 100.0
                );
                Ok((stream_bits, delay))
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
        for _ in 0..8 {
            let available_words = self.num_words_in_fifo();
            if available_words == 0 {
                return;
            }
            let byte_count = (available_words as usize)
                .saturating_mul(2)
                .min(USB3_BLOCK_SIZE_BYTES);
            let mut buffer = vec![0_u8; byte_count];
            let _ = self
                .device
                .read_from_block_pipe_out(PIPE_OUT_DATA, USB3_BLOCK_SIZE_BYTES, &mut buffer);
        }
    }

    fn num_words_in_fifo(&self) -> u32 {
        self.device.update_wire_outs();
        self.device.get_wire_out_value(WIRE_OUT_NUM_WORDS)
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
            Self::SpiStillRunning => write!(formatter, "Rhythm SPI run did not stop before timeout"),
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
