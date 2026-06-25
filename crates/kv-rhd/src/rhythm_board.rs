//! Rhythm/Keyvast FPGA data-plane board: FPGA bring-up and configuration.

use std::path::Path;
use std::thread;
use std::time::Duration;

use crate::backend::RhdReadError;
use crate::commands::{AuxCommandSlot, BoardPort, RHD_ADC_CALIBRATION_SAMPLES, Rhd2000Registers};
use crate::frame_analysis::{amplifier_mean_raw_word, aux_command_trigger_bit};
use crate::frontpanel::FrontPanelDevice;
use crate::protocol::{
    CHANNELS_PER_STREAM, DEFAULT_RHD_SAMPLE_RATE, MAX_SUPPORTED_STREAMS, RHYTHM_BOARD_ID,
    RhdChipType, USB3_BLOCK_SIZE_BYTES,
};

pub(crate) const WIRE_IN_RESET_RUN: i32 = 0x00;
// Open Ephys Rhythm USB3 writes the full 32-bit maxTimeStep to a SINGLE WireIn
// (0x01). WireIn 0x02 is SerialDigitalInCntl, NOT a maxTimeStep high word —
// writing the high bits there corrupted SPI/digital control and silenced the
// amplifier MISO during continuous acquisition (maxTimeStep = u32::MAX), while
// the short scan/calibration runs (small maxTimeStep, high word 0) worked.
pub(crate) const WIRE_IN_MAX_TIME_STEP: i32 = 0x01;
pub(crate) const WIRE_IN_DATA_FREQ_PLL: i32 = 0x03;
pub(crate) const WIRE_IN_MISO_DELAY: i32 = 0x04;
pub(crate) const WIRE_IN_CMD_RAM_ADDR: i32 = 0x05;
pub(crate) const WIRE_IN_CMD_RAM_BANK: i32 = 0x06;
pub(crate) const WIRE_IN_CMD_RAM_DATA: i32 = 0x07;
pub(crate) const WIRE_IN_AUX_CMD_BANK1: i32 = 0x08;
pub(crate) const WIRE_IN_AUX_CMD_BANK2: i32 = 0x09;
pub(crate) const WIRE_IN_AUX_CMD_BANK3: i32 = 0x0a;
pub(crate) const WIRE_IN_AUX_CMD_LENGTH: i32 = 0x0b;
pub(crate) const WIRE_IN_AUX_CMD_LOOP: i32 = 0x0c;
pub(crate) const WIRE_IN_DATA_STREAM_SEL_1234: i32 = 0x12;
pub(crate) const WIRE_IN_DATA_STREAM_SEL_5678: i32 = 0x13;
pub(crate) const WIRE_IN_DATA_STREAM_EN: i32 = 0x14;
pub(crate) const WIRE_IN_TTL_OUT: i32 = 0x15;
// Board analog/auxiliary plane (Intan Rhythm USB3). Open Ephys initializes these
// to known-safe states in initializeBoard(); the GUI previously left them at FPGA
// power-up defaults. DAC re-referencing (WireInDacReref) subtracts a common
// reference from EVERY amplifier channel when enabled, and external fast settle
// can hold the amplifiers in reset -- both corrupt all channels (including ones
// shorted to REF) if left active.
pub(crate) const WIRE_IN_DAC_REREF: i32 = 0x0e;
pub(crate) const WIRE_IN_DAC_SOURCE_BASE: i32 = 0x16; // WireInDacSource1..8 = 0x16..=0x1d
pub(crate) const WIRE_IN_DAC_MANUAL: i32 = 0x1e;
pub(crate) const WIRE_IN_MULTI_USE: i32 = 0x1f;
pub(crate) const WIRE_OUT_NUM_WORDS_LSB: i32 = 0x20;
pub(crate) const WIRE_OUT_NUM_WORDS_MSB: i32 = 0x26;
pub(crate) const WIRE_OUT_SPI_RUNNING: i32 = 0x22;
pub(crate) const WIRE_OUT_DATA_CLK_LOCKED: i32 = 0x24;
pub(crate) const WIRE_OUT_BOARD_ID: i32 = 0x3e;
pub(crate) const WIRE_OUT_BOARD_VERSION: i32 = 0x3f;
pub(crate) const TRIG_IN_CONFIG: i32 = 0x40;
pub(crate) const TRIG_IN_SPI_START: i32 = 0x41;
pub(crate) const TRIG_IN_DAC_CONFIG: i32 = 0x42;
pub(crate) const PIPE_OUT_DATA: i32 = 0xa0;
pub(crate) const RAM_BURST_SIZE: u32 = 256;

// KeyVast headstage power comes up over I2C (the MicroBlaze control plane)
// *after* FPGA configuration: eFuse -> per-module DCDC -> per-module isolated
// power, taking ~0.6 s and retried on a ~6 s cadence. The RHD scan must wait
// for the rails and retry until the chip answers, rather than probing once
// while the headstage is still unpowered (which silently fell back to Port A /
// delay 0 -- the wrong MISO phase -- yielding half-scale 0x4000 data).
const HEADSTAGE_POWER_SETTLE_MS: u64 = 1200;
const SCAN_RETRY_MS: u64 = 600;
// Total scan budget = HEADSTAGE_POWER_SETTLE_MS + SCAN_MAX_ATTEMPTS*SCAN_RETRY_MS
// must comfortably exceed the worst-case KeyVast I2C power bring-up (~6 s retry
// cadence). 1200 + 12*600 = 8.4 s leaves margin so a single missed cadence
// window can't exhaust attempts before the rails are up.
const SCAN_MAX_ATTEMPTS: u32 = 12;

pub(crate) struct RhythmFrontPanelBoard {
    pub(crate) device: FrontPanelDevice,
    pub(crate) board_id: u32,
    pub(crate) board_version: u32,
}

impl RhythmFrontPanelBoard {
    pub(crate) fn configure(
        device: FrontPanelDevice,
        bitfile_path: &Path,
        enabled_streams: usize,
        cable_length_meters: f64,
    ) -> Result<(Self, usize), RhdReadError> {
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
        board.reset_board_analog_state()?;
        board.set_cable_length_meters(0, cable_length_meters)?;
        if enabled_streams > 1 {
            board.set_cable_length_meters(1, cable_length_meters)?;
        }
        board.enable_streams(enabled_streams)?;
        board.set_default_data_sources()?;
        board.clear_ttl_out()?;
        log::info!("data plane configured; initializing RHD chips (ADC calibration)...");
        let detected_streams = board.initialize_rhd_chips(enabled_streams)?;
        board.set_max_time_step(u32::MAX)?;
        board.set_continuous_run_mode(true)?;
        board.flush_fifo()?;
        log::info!("board configured and armed for continuous acquisition");

        Ok((board, detected_streams))
    }

    /// Put the board's analog/auxiliary plane into a known-safe state, mirroring
    /// Open Ephys `Rhd2000EvalBoardUsb3::initializeBoard`. The GUI previously
    /// relied on FPGA power-up defaults for these; if DAC re-referencing or
    /// external fast settle come up enabled, they corrupt every amplifier channel
    /// (a common reference subtracted from all channels, or periodic amplifier
    /// resets) — visible even on channels shorted to REF. Uses the stock Intan
    /// Rhythm USB3 WireIn/Trigger map (the loaded bitstream is intan_rec_controller).
    fn reset_board_analog_state(&self) -> Result<(), RhdReadError> {
        // Disable hardware DAC re-referencing (WireInDacReref bit 0x400).
        self.device
            .set_wire_in_value(WIRE_IN_DAC_REREF, 0x0000_0000, 0x0000_0400)
            .map_err(RhdReadError::FrontPanel)?;
        // Disable all 8 board DACs (WireInDacSource1..8 bit 0x800).
        for dac in 0..8 {
            self.device
                .set_wire_in_value(WIRE_IN_DAC_SOURCE_BASE + dac, 0x0000_0000, 0x0000_0800)
                .map_err(RhdReadError::FrontPanel)?;
        }
        // Park the manual DAC at midscale; zero DAC gain and audio noise-suppress
        // (masked fields of WireInResetRun, leaving reset/run/dsp-settle bits intact).
        self.device
            .set_wire_in_value(WIRE_IN_DAC_MANUAL, 32_768, u32::MAX)
            .map_err(RhdReadError::FrontPanel)?;
        self.device
            .set_wire_in_value(WIRE_IN_RESET_RUN, 0, 0xe000)
            .map_err(RhdReadError::FrontPanel)?;
        self.device
            .set_wire_in_value(WIRE_IN_RESET_RUN, 0, 0x1fc0)
            .map_err(RhdReadError::FrontPanel)?;
        self.device.update_wire_ins();

        // Disable external (TTL-triggered) fast settle.
        self.device
            .set_wire_in_value(WIRE_IN_MULTI_USE, 0, u32::MAX)
            .map_err(RhdReadError::FrontPanel)?;
        self.device.update_wire_ins();
        self.device
            .activate_trigger_in(TRIG_IN_CONFIG, 6)
            .map_err(RhdReadError::FrontPanel)?;

        // Disable external digital-out on all 8 ports (TrigInDacConfig bits 16..23).
        for port in 0..8 {
            self.device
                .set_wire_in_value(WIRE_IN_MULTI_USE, 0, u32::MAX)
                .map_err(RhdReadError::FrontPanel)?;
            self.device.update_wire_ins();
            self.device
                .activate_trigger_in(TRIG_IN_DAC_CONFIG, 16 + port)
                .map_err(RhdReadError::FrontPanel)?;
        }

        log::info!("board analog plane reset: DAC re-ref off, DACs off, external fast-settle off");
        Ok(())
    }

    pub(crate) fn reset_board(&self) -> Result<(), RhdReadError> {
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
    pub(crate) fn set_sample_rate(&self, sample_rate: f64) -> Result<bool, RhdReadError> {
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

        self.wait_for_dcm_done()?;
        self.device
            .set_wire_in_value(WIRE_IN_DATA_FREQ_PLL, 256 * m + d, u32::MAX)
            .map_err(RhdReadError::FrontPanel)?;
        self.device.update_wire_ins();
        self.device
            .activate_trigger_in(TRIG_IN_CONFIG, 0)
            .map_err(RhdReadError::FrontPanel)?;
        self.wait_for_data_clock_locked()?;

        Ok(true)
    }

    pub(crate) fn set_sample_rate_30khz(&self) -> Result<bool, RhdReadError> {
        self.set_sample_rate(30000.0)
    }

    pub(crate) fn set_max_time_step(&self, max_time_step: u32) -> Result<(), RhdReadError> {
        self.device
            .set_wire_in_value(WIRE_IN_MAX_TIME_STEP, max_time_step, u32::MAX)
            .map_err(RhdReadError::FrontPanel)?;
        self.device.update_wire_ins();
        Ok(())
    }

    pub(crate) fn set_continuous_run_mode(&self, enabled: bool) -> Result<(), RhdReadError> {
        self.device
            .set_wire_in_value(WIRE_IN_RESET_RUN, if enabled { 0x02 } else { 0x00 }, 0x02)
            .map_err(RhdReadError::FrontPanel)?;
        self.device.update_wire_ins();
        Ok(())
    }

    pub(crate) fn enable_streams(&self, enabled_streams: usize) -> Result<(), RhdReadError> {
        let mask = crate::protocol::stream_enable_mask(enabled_streams);
        self.device
            .set_wire_in_value(WIRE_IN_DATA_STREAM_EN, mask, u32::MAX)
            .map_err(RhdReadError::FrontPanel)?;
        self.device.update_wire_ins();
        Ok(())
    }

    pub(crate) fn clear_ttl_out(&self) -> Result<(), RhdReadError> {
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
    pub(crate) fn set_data_source(&self, stream: u32, source: u32) -> Result<(), RhdReadError> {
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
    pub(crate) fn set_default_data_sources(&self) -> Result<(), RhdReadError> {
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

    pub(crate) fn set_dsp_settle(&self, enabled: bool) -> Result<(), RhdReadError> {
        self.device
            .set_wire_in_value(WIRE_IN_RESET_RUN, if enabled { 0x04 } else { 0x00 }, 0x04)
            .map_err(RhdReadError::FrontPanel)?;
        self.device.update_wire_ins();
        Ok(())
    }

    pub(crate) fn initialize_rhd_chips(
        &self,
        enabled_streams: usize,
    ) -> Result<usize, RhdReadError> {
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
        // Wait for the headstage power rails (I2C bring-up) before probing, then
        // re-scan until a chip answers -- the rails can lag FPGA config by up to a
        // few seconds, and probing too early is exactly what dropped acquisition
        // onto Port A / delay 0 (half-scale 0x4000 data).
        thread::sleep(Duration::from_millis(HEADSTAGE_POWER_SETTLE_MS));
        let mut attempt = 0_u32;
        let (port, delay, chip_id, found) = loop {
            attempt += 1;
            let result = self.scan_ports_for_headstage(enabled_streams)?;
            if result.3 {
                log::info!("headstage located on scan attempt {attempt}");
                break result;
            }
            if attempt >= SCAN_MAX_ATTEMPTS {
                log::error!(
                    "no responding RHD chip found after {attempt} scan attempts. Refusing to \
                     arm acquisition on the Port A / delay 0 fallback (that records half-scale \
                     0x4000 / flat data). Check the headstage is connected and powered, and \
                     that this is a KeyVast bitstream."
                );
                return Err(RhdReadError::HeadstageNotFound);
            }
            log::info!(
                "scan attempt {attempt}: headstage not responding yet (power may still be \
                 ramping); retrying in {} ms",
                SCAN_RETRY_MS
            );
            thread::sleep(Duration::from_millis(SCAN_RETRY_MS));
        };

        // Auto-detect the channel count from the chip's register-63 ID: RHD2164
        // is dual-MISO (2 streams / 64ch); RHD2132 (32ch) and RHD2216 (16ch) are
        // single-stream. Fall back to the requested count when nothing answered.
        let chip = chip_id.and_then(|id| RhdChipType::from_register63(id as u16));
        let detected_streams = match chip {
            Some(c) => c.streams_per_headstage(),
            None => enabled_streams.clamp(1, MAX_SUPPORTED_STREAMS),
        };
        if found {
            log::info!(
                "auto-detected chip {:?} -> {} data stream(s), {} amplifier channel(s)",
                chip,
                detected_streams,
                detected_streams * CHANNELS_PER_STREAM,
            );
        }
        let stream_mask = ((1_u32 << detected_streams) - 1) << (port as u32 * 4);
        self.enable_stream_mask(stream_mask)?;
        self.set_cable_delay_all_ports(delay)?;

        // Final ADC calibration at the chosen delay, then flush the calibration run.
        self.set_max_time_step(RHD_ADC_CALIBRATION_SAMPLES as u32)?;
        self.set_continuous_run_mode(false)?;
        self.run()?;
        self.wait_until_not_running()?;
        self.read_and_discard_samples(detected_streams, RHD_ADC_CALIBRATION_SAMPLES)?;

        self.select_aux_command_bank_all_ports(AuxCommandSlot::AuxCmd3, 1)?;

        // Defense-in-depth: confirm the committed MISO delay actually yields
        // DSP-centered amplifier data (~0x8000). A mean near 0x4000 means the
        // 16-bit word is sampled one SPI bit early (wrong sampling phase) -- the
        // exact half-scale failure the power-settle/retry scan exists to prevent.
        // The chip-ID check is a strong proxy for this, but promote the existing
        // 0x4000 diagnostic from logging-only to a hard gate at commit time.
        const VERIFY_SAMPLES: usize = 128;
        self.set_max_time_step(VERIFY_SAMPLES as u32)?;
        self.set_continuous_run_mode(false)?;
        self.run()?;
        self.wait_until_not_running()?;
        let verify_raw = self.read_pipe_block(detected_streams, VERIFY_SAMPLES)?;
        if let Some(mean) =
            amplifier_mean_raw_word(&verify_raw, detected_streams, VERIFY_SAMPLES, 0)
        {
            log::info!("post-delay centering check: amp mean raw word = 0x{mean:04x}");
            if (0x3000..=0x5000).contains(&mean) {
                log::error!(
                    "amplifier data is half-scale (mean 0x{mean:04x} ~ 0x4000): the committed \
                     MISO delay is the wrong sampling phase. Refusing to record corrupt data."
                );
                return Err(RhdReadError::HalfScaleAmplifierData {
                    mean_raw_word: mean,
                });
            }
        }

        Ok(detected_streams)
    }

    pub(crate) fn upload_command_list(
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

    pub(crate) fn select_aux_command_bank_all_ports(
        &self,
        slot: AuxCommandSlot,
        bank: u8,
    ) -> Result<(), RhdReadError> {
        for port in BoardPort::all() {
            self.select_aux_command_bank(port, slot, bank)?;
        }
        Ok(())
    }

    pub(crate) fn select_aux_command_bank(
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

    pub(crate) fn select_aux_command_length(
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

    pub(crate) fn set_cable_length_meters(
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
}
