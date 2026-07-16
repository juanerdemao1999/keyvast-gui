//! Rhythm/Keyvast FPGA data-plane board: acquisition, FIFO management and the
//! MISO delay scan used to locate a connected headstage.

use std::thread;
use std::time::Duration;

use crate::backend::RhdReadError;
use crate::frame_analysis::{
    amplifier_mean_raw_word, auxcmd3_bytes, first_amp_words, hex_bytes, hex_words,
    min_stream_railed_fraction, probe_chip_id, probe_frame_integrity, stream_range_label,
    verify_chip_id_in_probe,
};
use crate::protocol::{RhythmDataConfig, USB3_BLOCK_SIZE_BYTES, bytes_per_block};
use crate::rhythm_board::RhythmFrontPanelBoard;
use crate::rhythm_board::*;

impl RhythmFrontPanelBoard {
    pub(crate) fn run(&self) -> Result<(), RhdReadError> {
        self.device
            .activate_trigger_in(TRIG_IN_SPI_START, 0)
            .map_err(RhdReadError::FrontPanel)
    }

    pub(crate) fn start_continuous_acquisition(&self) -> Result<(), RhdReadError> {
        self.set_max_time_step(u32::MAX)?;
        self.set_continuous_run_mode(true)?;
        self.run()
    }

    pub(crate) fn read_raw_block(
        &self,
        config: &RhythmDataConfig,
    ) -> Result<Vec<u8>, RhdReadError> {
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

    pub(crate) fn read_and_discard_samples(
        &self,
        enabled_streams: usize,
        samples: usize,
    ) -> Result<(), RhdReadError> {
        self.read_pipe_block(enabled_streams, samples)?;
        Ok(())
    }

    pub(crate) fn read_pipe_block(
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

    pub(crate) fn set_cable_delay_all_ports(&self, delay: u32) -> Result<(), RhdReadError> {
        let delay = delay.min(15);
        let mut value = 0_u32;
        for port in 0..8 {
            value |= delay << (port * 4);
        }
        log::debug!("set_cable_delay_all_ports delay={delay} (WireIn 0x04={value:#010x})");
        self.device
            .set_wire_in_value(WIRE_IN_MISO_DELAY, value, 0xffff_ffff)
            .map_err(RhdReadError::FrontPanel)?;
        self.device.update_wire_ins();
        Ok(())
    }

    /// Set MISO delay for a single port (0=A … 7=H) without affecting others.
    pub(crate) fn set_cable_delay_port(&self, port: usize, delay: u32) -> Result<(), RhdReadError> {
        let delay = delay.min(15);
        let shift = (port as u32) * 4;
        log::debug!("set_cable_delay_port port={port} delay={delay}");
        self.device
            .set_wire_in_value(WIRE_IN_MISO_DELAY, delay << shift, 0x0f << shift)
            .map_err(RhdReadError::FrontPanel)?;
        self.device.update_wire_ins();
        Ok(())
    }

    /// Write an arbitrary data-stream enable mask to the FPGA. Bit `i` enables
    /// physical data stream `i` (stream `i` belongs to SPI port `i / 4`).
    pub(crate) fn enable_stream_mask(&self, mask: u32) -> Result<(), RhdReadError> {
        log::debug!("enable_stream_mask {mask:#010x} (WireIn 0x14)");
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
    /// port and, like Open Ephys scanPorts, pick the second good delay
    /// (indexSecondGoodDelay) for timing margin.
    ///
    /// Each probe enables exactly `enabled_streams` streams (the same count
    /// acquisition will use), so the FPGA frame size during the scan matches what the
    /// parser expects — only the *physical* port behind those stream slots changes.
    /// AuxCmd3 bank 0 (register config + ADC calibrate) must be selected so each run
    /// also configures/calibrates the chip. Returns `(port, delay, chip-ID byte,
    /// found)`; `found == false` means nothing responded (the caller decides whether
    /// to retry or refuse to arm).
    pub(crate) fn scan_ports_for_headstage(
        &self,
        enabled_streams: usize,
    ) -> Result<(usize, u32, Option<u8>, bool), RhdReadError> {
        const PROBE_SAMPLES: usize = 128;
        const PORT_LETTERS: [char; 8] = ['A', 'B', 'C', 'D', 'E', 'F', 'G', 'H'];

        let stream_bits = crate::protocol::stream_enable_mask(enabled_streams);

        // (port index, chosen delay, has_chip_id, chip-ID byte)
        let mut best: Option<(usize, u32, bool, Option<u8>)> = None;

        for (port, &port_letter) in PORT_LETTERS.iter().enumerate() {
            let first_stream = (port * 4) as u32;
            self.enable_stream_mask(stream_bits << first_stream)?;

            // Delays where chip ID was verified (strong validation).
            let mut id_verified_delays: Vec<u32> = Vec::new();
            // Delays where railed fraction < 50% (weak fallback).
            let mut low_railed_delays: Vec<u32> = Vec::new();
            // First register-63 chip ID seen on a chip-ID-verified delay.
            let mut port_chip_id: Option<u8> = None;
            // Lowest railed fraction seen on this port, so the always-on per-port
            // summary reports *something* even when a bitstream is fully silent.
            let mut port_min_railed = f64::INFINITY;

            for delay in 0..16_u32 {
                self.set_cable_delay_all_ports(delay)?;
                self.set_max_time_step(PROBE_SAMPLES as u32)?;
                self.set_continuous_run_mode(false)?;
                self.run()?;
                self.wait_until_not_running()?;
                let raw_full = self.read_pipe_block(enabled_streams, PROBE_SAMPLES)?;

                // Resync to the frame magic. The probe read can start mid-frame:
                // residual FIFO bytes ahead of the run shift the whole block, and
                // that offset moves the AuxCmd3 word so the chip ID never decodes and
                // the railed/mean gates sample the wrong slots (observed: a 4-byte
                // shift => chip_id=None on a live chip, and delay picked off the
                // half-scale phase). Dropping the leading partial frame puts the magic
                // back at offset 0 for all analysis below. Normal continuous
                // acquisition avoids this because configure() flushes the FIFO before
                // it streams; the scan runs before that flush.
                let frame_bytes =
                    crate::protocol::FrameLayout::new(enabled_streams).bytes_per_frame();
                let sync_off = crate::frame_analysis::magic_offsets(&raw_full, 1)
                    .first()
                    .copied()
                    .unwrap_or(0)
                    .min(raw_full.len());
                let raw: &[u8] = &raw_full[sync_off..];
                let probe_samples = (raw.len() / frame_bytes).min(PROBE_SAMPLES);

                // On the very first probe read, log a frame-integrity check. Being
                // independent of the MISO phase, it separates "the FPGA is not
                // emitting valid Rhythm frames" (real bitstream/endpoint fault) from a
                // pure read-misalignment that the resync above already recovers.
                if port == 0 && delay == 0 {
                    let recovered = probe_frame_integrity(raw, enabled_streams, probe_samples).0;
                    if recovered && sync_off == 0 {
                        log::info!("probe frame check OK: aligned, {probe_samples} frames");
                    } else if recovered {
                        log::info!(
                            "probe frame check: read was MISALIGNED by {sync_off} B (stale FIFO \
                             ahead of the run) but frame size is correct — auto-resynced to the \
                             frame boundary; {probe_samples} frames usable"
                        );
                    } else {
                        let (_, summary) =
                            probe_frame_integrity(&raw_full, enabled_streams, PROBE_SAMPLES);
                        log::warn!(
                            "probe frame check PROBLEM (resync did not recover): {summary} — a \
                             frame-size or magic mismatch means the FPGA data plane is not emitting \
                             the expected Rhythm frames (bitstream/endpoint issue, not MISO delay)"
                        );
                    }
                }

                let has_id = verify_chip_id_in_probe(raw, enabled_streams, probe_samples);
                let railed = min_stream_railed_fraction(raw, enabled_streams, probe_samples);
                let chip_id = probe_chip_id(raw, enabled_streams, probe_samples, 0);
                port_min_railed = port_min_railed.min(railed);

                // Drive delay selection off the FULL register-63 chip ID
                // (`chip_id.is_some()`), NOT the lenient INTAN ROM marker (`has_id`).
                // On marginal MISO phases the ROM marker can still be decoded while the
                // chip-ID register cannot; those phases yield half-scale 0x4000
                // amplifier data after ADC calibration and are then rejected by the
                // centering gate at commit time. Gating on the chip ID keeps the
                // candidate run on solidly-locked phases (e.g. delays 4-7 here, not the
                // marginal 2-3 that put the old `has_id` rule on the half-scale delay 3).
                if chip_id.is_some() {
                    id_verified_delays.push(delay);
                    if port_chip_id.is_none() {
                        port_chip_id = chip_id;
                    }
                }
                if railed < 0.5 {
                    low_railed_delays.push(delay);
                }

                // Per-delay detail. This was previously gated behind `railed < 0.9
                // || has_id || chip_id`, so a fully-railed / silent MISO (the exact
                // failure we are debugging) logged NOTHING for all 128 probes. Log
                // every delay unconditionally at debug so the failing case is fully
                // visible under `run-gui.bat debug` (RUST_LOG=info,kv_rhd=debug).
                if log::log_enabled!(log::Level::Debug) {
                    let amp_mean = amplifier_mean_raw_word(raw, enabled_streams, probe_samples, 0);
                    log::debug!(
                        "  scan port {} delay {:2}: has_id={} chip_id={:?} railed_s0={:.3} amp_mean_raw_s0={}",
                        port_letter,
                        delay,
                        has_id,
                        chip_id,
                        railed,
                        amp_mean
                            .map(|m| format!("0x{m:04x}"))
                            .unwrap_or_else(|| "n/a".to_string()),
                    );
                    // Raw MISO forensics: the AuxCmd3 register-readback bytes (look
                    // for ASCII "INTAN" = 49 4e 54 41 4e, with the reg-63 chip-ID byte
                    // 13 words before it) plus the first amplifier words. All `ff` =
                    // idle/unpowered line; shifted "INTAN" = wrong sampling phase.
                    let aux = auxcmd3_bytes(raw, enabled_streams, probe_samples, 0);
                    let take = aux.len().min(24);
                    let amp = first_amp_words(raw, enabled_streams, probe_samples, 0, 4);
                    log::debug!(
                        "      aux3_bytes[0..{take}]={}  amp_s0[0..4]={}",
                        hex_bytes(&aux[..take]),
                        hex_words(&amp),
                    );
                }
            }

            // Prefer chip-ID-verified delays; fall back to low-railed delays.
            let (good_delays, validated_by_id) = if !id_verified_delays.is_empty() {
                (id_verified_delays, true)
            } else {
                (low_railed_delays, false)
            };

            // good_delays is now the chip-ID-confirmed run. Match Open Ephys
            // DeviceThread::scanPorts: 1-2 good delays -> the first; >2 -> the SECOND
            // good delay (indexSecondGoodDelay), NOT the middle. good_delays is in
            // ascending order, so index 1 is the second. On this rig the confirmed run
            // is 4-7, so the second good delay is 5 (a solidly-locked phase) rather
            // than the marginal delay 3 the old INTAN-marker run selected.
            let chosen_delay = if good_delays.len() > 2 {
                good_delays[1]
            } else if let Some(&d) = good_delays.first() {
                d
            } else {
                // Always-on: a non-responding port used to `continue` silently, which
                // is why a failing bitfile produced almost no log. Report it.
                log::info!(
                    "port {} ({}): no responding chip (min railed {:.3}, no chip-ID, no \
                     low-railed delay)",
                    port_letter,
                    stream_range_label(first_stream, enabled_streams),
                    port_min_railed,
                );
                continue;
            };

            let method = if validated_by_id {
                "chip ID"
            } else {
                "railed fraction"
            };
            log::info!(
                "port {} ({}): RESPONDING — {} good delays via {} @ chosen delay {} \
                 (min railed {:.3}, chip_id={:?})",
                port_letter,
                stream_range_label(first_stream, enabled_streams),
                good_delays.len(),
                method,
                chosen_delay,
                port_min_railed,
                port_chip_id,
            );

            // Prefer chip-ID-verified ports over railed-fraction-only ports.
            let dominated = best
                .as_ref()
                .is_some_and(|&(_, _, prev_id, _)| prev_id && !validated_by_id);
            if !dominated
                && best
                    .as_ref()
                    .is_none_or(|&(_, _, prev_id, _)| validated_by_id >= prev_id)
            {
                best = Some((port, chosen_delay, validated_by_id, port_chip_id));
            }
        }

        match best {
            Some((port, delay, _, chip_id)) => {
                let first_stream = (port * 4) as u32;
                log::info!(
                    "FOUND headstage on port {} ({}) at MISO delay {} (chip ID {:?})",
                    PORT_LETTERS[port],
                    stream_range_label(first_stream, enabled_streams),
                    delay,
                    chip_id,
                );
                // Apply per-port delay only for the discovered port.
                self.set_cable_delay_port(port, delay)?;
                Ok((port, delay, chip_id, true))
            }
            None => Ok((0, 0, None, false)),
        }
    }

    pub(crate) fn wait_for_fifo_words(&self, needed_words: u32) -> Result<(), RhdReadError> {
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

    pub(crate) fn flush_fifo(&self) -> Result<(), RhdReadError> {
        // A plausible FIFO fill is at most tens of thousands of 16-bit words (the
        // pipe BRAM FIFO is ~16 K words). A value far above this means the num_words
        // WireOut pair (0x20 LSW / 0x26 MSW) does not report fill on this bitstream
        // (e.g. a build that repurposes 0x26) — draining against a garbage count
        // would spin the loops and, worse, size a multi-GB read buffer. Treat an
        // implausible count as "unknown" and stop rather than hang.
        const MAX_PLAUSIBLE_FIFO_WORDS: u32 = 1 << 20; // 1 M words = 2 MB

        // Diagnostic: dump the raw fill registers so a bogus MSW is obvious.
        self.device.update_wire_outs();
        let raw_lsb = self.device.get_wire_out_value(WIRE_OUT_NUM_WORDS_LSB);
        let raw_msb = self.device.get_wire_out_value(WIRE_OUT_NUM_WORDS_MSB);
        log::debug!(
            "flush_fifo: entry — WireOut 0x20(lsw)={raw_lsb:#06x} 0x26(msw)={raw_msb:#06x} => {} words",
            self.num_words_in_fifo()
        );

        // Set USB3 pipeout block-throttle override (bit 16 of WireInResetRun)
        // so the FPGA allows reads of any size during flush.
        self.device
            .set_wire_in_value(WIRE_IN_RESET_RUN, 1 << 16, 1 << 16)
            .map_err(RhdReadError::FrontPanel)?;
        self.device.update_wire_ins();

        // Phase A: bulk drain with large reads (up to 256 KB per iteration).
        const FLUSH_CHUNK: usize = 256 * 1024;
        let mut phase_a = 0_u32;
        for _ in 0..10_000 {
            let available_words = self.num_words_in_fifo();
            if available_words > MAX_PLAUSIBLE_FIFO_WORDS {
                log::warn!(
                    "flush_fifo: fill count {available_words} words implausible (raw 0x20={raw_lsb:#06x} \
                     0x26={raw_msb:#06x}) — num_words WireOut likely unimplemented on this bitstream; \
                     aborting drain"
                );
                break;
            }
            if (available_words as usize) < FLUSH_CHUNK / 2 {
                break;
            }
            phase_a += 1;
            if phase_a <= 3 || phase_a.is_multiple_of(500) {
                log::debug!("flush_fifo: phaseA iter {phase_a}, {available_words} words");
            }
            let mut buffer = vec![0_u8; FLUSH_CHUNK];
            let _ = self.device.read_from_block_pipe_out(
                PIPE_OUT_DATA,
                USB3_BLOCK_SIZE_BYTES,
                &mut buffer,
            );
        }
        log::debug!(
            "flush_fifo: phaseA done ({phase_a} iters), {} words left",
            self.num_words_in_fifo()
        );

        // Phase B: drain remaining with appropriately-sized reads.
        let mut phase_b = 0_u32;
        for _ in 0..10_000 {
            let available_words = self.num_words_in_fifo();
            if available_words == 0 {
                break;
            }
            if available_words > MAX_PLAUSIBLE_FIFO_WORDS {
                log::warn!(
                    "flush_fifo: phaseB fill count {available_words} words implausible — aborting drain"
                );
                break;
            }
            let byte_count = (available_words as usize).saturating_mul(2);
            // Round up to USB3_BLOCK_SIZE_BYTES boundary.
            let aligned = byte_count.div_ceil(USB3_BLOCK_SIZE_BYTES).max(1) * USB3_BLOCK_SIZE_BYTES;
            phase_b += 1;
            if phase_b <= 3 || phase_b.is_multiple_of(500) {
                log::debug!(
                    "flush_fifo: phaseB iter {phase_b}, {available_words} words, reading {aligned} B"
                );
            }
            let mut buffer = vec![0_u8; aligned];
            let _ = self.device.read_from_block_pipe_out(
                PIPE_OUT_DATA,
                USB3_BLOCK_SIZE_BYTES,
                &mut buffer,
            );
        }
        log::debug!("flush_fifo: phaseB done ({phase_b} iters)");

        // Release throttle override.
        self.device
            .set_wire_in_value(WIRE_IN_RESET_RUN, 0, 1 << 16)
            .map_err(RhdReadError::FrontPanel)?;
        self.device.update_wire_ins();

        let remaining = self.num_words_in_fifo();
        log::debug!("flush_fifo: exit — {remaining} words remaining");
        if remaining > 0 && remaining <= MAX_PLAUSIBLE_FIFO_WORDS {
            return Err(RhdReadError::FifoFlushIncomplete {
                remaining_words: remaining,
            });
        }
        Ok(())
    }

    pub(crate) fn num_words_in_fifo(&self) -> u32 {
        self.device.update_wire_outs();
        // The KeyVast/demo data plane reports the full pipe-FIFO fill in the single
        // 32-bit WireOut 0x20 (occ16) and repurposes 0x26 for other status (e.g. the
        // vUART). Combining 0x20 with 0x26 as a 32-bit count (the stock Intan Rhythm
        // WireOutNumWordsLsb/Msb convention) then yields a garbage multi-million-word
        // value (observed 0x26=0x800040 => 4.19 M words) that hangs flush_fifo and
        // makes wait_for_fifo_words return instantly on misaligned data. Read 0x20
        // alone: the pipe FIFO is ~16 K words, well under 2^16, so the low word is the
        // whole count for both bitstream families during block-sized reads.
        self.device.get_wire_out_value(WIRE_OUT_NUM_WORDS_LSB)
    }

    pub(crate) fn wait_for_dcm_done(&self) -> Result<(), RhdReadError> {
        for _ in 0..100 {
            self.device.update_wire_outs();
            if self.device.get_wire_out_value(WIRE_OUT_DATA_CLK_LOCKED) & 0x02 != 0 {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(1));
        }
        Err(RhdReadError::PllDcmTimeout)
    }

    pub(crate) fn wait_for_data_clock_locked(&self) -> Result<(), RhdReadError> {
        for _ in 0..100 {
            self.device.update_wire_outs();
            if self.device.get_wire_out_value(WIRE_OUT_DATA_CLK_LOCKED) & 0x01 != 0 {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(1));
        }
        Err(RhdReadError::PllLockTimeout)
    }

    pub(crate) fn wait_until_not_running(&self) -> Result<(), RhdReadError> {
        for _ in 0..200 {
            if !self.is_running() {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(5));
        }

        Err(RhdReadError::SpiStillRunning)
    }

    #[allow(dead_code)]
    pub(crate) fn is_running(&self) -> bool {
        self.device.update_wire_outs();
        self.device.get_wire_out_value(WIRE_OUT_SPI_RUNNING) & 0x01 != 0
    }

    #[allow(dead_code)]
    pub(crate) fn board_id(&self) -> u32 {
        self.board_id
    }

    #[allow(dead_code)]
    pub(crate) fn board_version(&self) -> u32 {
        self.board_version
    }

    // ------------------------------------------------------------------
    // Unimplemented FPGA control features.
    //
    // Open Ephys' `rhd2000evalboard` exposes these on the stock Intan
    // RhythmUSB3 (XEM7310) bitstream, but the KeyVast bitstream
    // (`keyvast_*.bit`) re-routes the SPI buses through the module-IO ring
    // and has not been confirmed to map these WireIn endpoints to the same
    // addresses. Per project rule 1, the exact endpoint addresses are TBD
    // until verified against the running KeyVast FPGA, so these are
    // deliberately no-ops (returning `Ok(())`) rather than writing a guessed
    // address that could collide with another control register.
    //
    // Each stub documents the Open Ephys reference endpoint so the wiring is
    // a lookup-and-confirm step once hardware is available. They are marked
    // `#[allow(dead_code)]` until the GUI or a command interface calls them.
    // ------------------------------------------------------------------

    /// Set an on-chip DAC threshold/level (used for the impedance-check
    /// waveform and spike-threshold DAC outputs).
    ///
    /// Open Ephys reference: `setDacThreshold` drives `WireInDacSource1..8`
    /// (one endpoint per DAC) plus `WireInDacManual` and a `TrigIn` strobe.
    /// KeyVast endpoint addresses unconfirmed — see module note above.
    #[allow(dead_code)]
    pub(crate) fn set_dac_threshold(
        &self,
        _dac_channel: u8,
        _threshold: u16,
    ) -> Result<(), RhdReadError> {
        Ok(())
    }

    /// Enable/disable an on-board status LED.
    ///
    /// Open Ephys reference: `setLedDisplay` writes the 8-bit LED bitmask to
    /// the `WireInLedDisplay` endpoint. KeyVast endpoint address unconfirmed —
    /// see module note above.
    #[allow(dead_code)]
    pub(crate) fn set_led(&self, _led_index: u8, _on: bool) -> Result<(), RhdReadError> {
        Ok(())
    }

    /// Trigger external fast-settle (amplifier blanking) on a logic channel.
    ///
    /// Open Ephys reference: `setExternalFastSettleChannel` writes the
    /// `WireInExternalFastSettle` endpoint (enable bit + channel select).
    /// KeyVast endpoint address unconfirmed — see module note above.
    #[allow(dead_code)]
    pub(crate) fn set_external_fast_settle_channel(
        &self,
        _channel: u8,
    ) -> Result<(), RhdReadError> {
        Ok(())
    }
}
