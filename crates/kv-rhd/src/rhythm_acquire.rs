//! Rhythm/Keyvast FPGA data-plane board: acquisition, FIFO management and the
//! MISO delay scan used to locate a connected headstage.

use std::thread;
use std::time::Duration;

use crate::backend::RhdReadError;
use crate::frame_analysis::{
    amplifier_mean_raw_word, min_stream_railed_fraction, probe_chip_id, stream_range_label,
    verify_chip_id_in_probe,
};
use crate::protocol::{RhythmDataConfig, USB3_BLOCK_SIZE_BYTES, bytes_per_block};
use crate::rhythm_board::RhythmFrontPanelBoard;
use crate::rhythm_board::*;

/// One MISO-delay probe result for a single port during the headstage scan.
#[derive(Clone, Copy, Debug)]
pub(crate) struct DelayProbe {
    /// MISO cable delay under test (0..16).
    pub delay: u32,
    /// Register-63 chip ID matched the expected RHD pattern at this delay.
    pub has_id: bool,
    /// Fraction of amplifier words pinned to a rail (idle-high 0xFFFF / 0x0000)
    /// for the quietest stream — 0.0 = clean, 1.0 = fully railed.
    pub railed: f64,
}

/// Maximum railed fraction tolerated for a probe to be *accepted* during the
/// port scan. A chip-ID match alone is not sufficient: a delay whose data is
/// mostly rail-pinned is the wrong sampling phase (or an open/dead line) and is
/// rejected even when register 63 happens to read back a valid chip ID (DA25).
pub(crate) const MAX_ACCEPT_RAILED_FRACTION: f64 = 0.5;

/// Pick the MISO delay for one port from its per-delay probes.
///
/// Returns `(chosen_delay, validated_by_id, railed_at_chosen)` or `None` when no
/// delay qualifies. A chip-ID-verified delay must *also* be below the railed
/// gate to count (DA25); only if no clean chip-ID delay exists do we fall back
/// to low-railed-only delays. `railed_at_chosen` is exposed so ports can be
/// ranked by signal quality rather than scan position (DA24).
pub(crate) fn choose_port_delay(probes: &[DelayProbe]) -> Option<(u32, bool, f64)> {
    let id_delays: Vec<&DelayProbe> = probes
        .iter()
        .filter(|p| p.has_id && p.railed < MAX_ACCEPT_RAILED_FRACTION)
        .collect();
    let (candidates, by_id) = if !id_delays.is_empty() {
        (id_delays, true)
    } else {
        let low: Vec<&DelayProbe> = probes
            .iter()
            .filter(|p| p.railed < MAX_ACCEPT_RAILED_FRACTION)
            .collect();
        (low, false)
    };
    // Open Ephys DeviceThread::scanPorts indexSecondGoodDelay: with >2 good
    // delays take the second (timing margin), otherwise the first. `probes` are
    // swept in ascending delay order, so `candidates` stays ordered.
    let chosen = if candidates.len() > 2 {
        candidates[1]
    } else {
        *candidates.first()?
    };
    Some((chosen.delay, by_id, chosen.railed))
}

/// Decide whether a freshly-scanned port should replace the current best (DA24).
///
/// Ranking is by validation tier first (a chip-ID-verified port always beats a
/// fraction-only port), then by signal quality (lower railed fraction at the
/// chosen delay) within the same tier. Scan order is never the tiebreaker, so a
/// later reflection/cross-talk port can no longer overwrite the real headstage
/// by last-wins.
pub(crate) fn port_is_better(
    cand_by_id: bool,
    cand_railed: f64,
    best_by_id: bool,
    best_railed: f64,
) -> bool {
    match (cand_by_id, best_by_id) {
        (true, false) => true,
        (false, true) => false,
        _ => cand_railed < best_railed,
    }
}

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
        self.device
            .set_wire_in_value(WIRE_IN_MISO_DELAY, delay << shift, 0x0f << shift)
            .map_err(RhdReadError::FrontPanel)?;
        self.device.update_wire_ins();
        Ok(())
    }

    /// Write an arbitrary data-stream enable mask to the FPGA. Bit `i` enables
    /// physical data stream `i` (stream `i` belongs to SPI port `i / 4`).
    pub(crate) fn enable_stream_mask(&self, mask: u32) -> Result<(), RhdReadError> {
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

        // (port index, chosen delay, has_chip_id, chip-ID byte, railed quality)
        let mut best: Option<(usize, u32, bool, Option<u8>, f64)> = None;

        for (port, &port_letter) in PORT_LETTERS.iter().enumerate() {
            let first_stream = (port * 4) as u32;
            self.enable_stream_mask(stream_bits << first_stream)?;

            // Per-delay probe results for this port (ascending delay order).
            let mut probes: Vec<DelayProbe> = Vec::with_capacity(16);
            // First register-63 chip ID seen on a chip-ID-verified delay.
            let mut port_chip_id: Option<u8> = None;

            for delay in 0..16_u32 {
                self.set_cable_delay_all_ports(delay)?;
                self.set_max_time_step(PROBE_SAMPLES as u32)?;
                self.set_continuous_run_mode(false)?;
                self.run()?;
                self.wait_until_not_running()?;
                let raw = self.read_pipe_block(enabled_streams, PROBE_SAMPLES)?;

                let has_id = verify_chip_id_in_probe(&raw, enabled_streams, PROBE_SAMPLES);
                let railed = min_stream_railed_fraction(&raw, enabled_streams, PROBE_SAMPLES);
                probes.push(DelayProbe {
                    delay,
                    has_id,
                    railed,
                });

                if railed < 0.9 || has_id {
                    let amp_mean = amplifier_mean_raw_word(&raw, enabled_streams, PROBE_SAMPLES, 0);
                    let chip_id = probe_chip_id(&raw, enabled_streams, PROBE_SAMPLES, 0);
                    log::info!(
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
                    // Only adopt a chip ID from a delay that also passes the
                    // railed gate, so a mostly-railed but ID-matching phase
                    // can't supply the reported chip ID (DA25).
                    if has_id && railed < MAX_ACCEPT_RAILED_FRACTION && port_chip_id.is_none() {
                        port_chip_id = chip_id;
                    }
                }
            }

            // Choose this port's delay with the railed gate applied per delay
            // (DA25). Returns the chosen delay's railed fraction for ranking.
            let Some((chosen_delay, validated_by_id, quality_railed)) = choose_port_delay(&probes)
            else {
                continue;
            };

            let method = if validated_by_id {
                "chip ID"
            } else {
                "railed fraction"
            };
            log::info!(
                "port {} ({}): chosen delay {} via {} (railed {:.3})  <- responding",
                port_letter,
                stream_range_label(first_stream, enabled_streams),
                chosen_delay,
                method,
                quality_railed,
            );

            // Rank ports by validation tier then signal quality, never by scan
            // position, so a later reflection/cross-talk port can't overwrite the
            // real headstage by last-wins (DA24).
            let replace = best
                .as_ref()
                .is_none_or(|&(_, _, best_id, _, best_railed)| {
                    port_is_better(validated_by_id, quality_railed, best_id, best_railed)
                });
            if replace {
                best = Some((
                    port,
                    chosen_delay,
                    validated_by_id,
                    port_chip_id,
                    quality_railed,
                ));
            }
        }

        match best {
            Some((port, delay, _, chip_id, _)) => {
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

        let remaining = self.num_words_in_fifo();
        if remaining > 0 {
            return Err(RhdReadError::FifoFlushIncomplete {
                remaining_words: remaining,
            });
        }
        Ok(())
    }

    pub(crate) fn num_words_in_fifo(&self) -> u32 {
        self.device.update_wire_outs();
        let msb = self.device.get_wire_out_value(WIRE_OUT_NUM_WORDS_MSB);
        let lsb = self.device.get_wire_out_value(WIRE_OUT_NUM_WORDS_LSB);
        (msb << 16) | (lsb & 0xFFFF)
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

#[cfg(test)]
mod scan_decision_tests {
    use super::*;

    fn probe(delay: u32, has_id: bool, railed: f64) -> DelayProbe {
        DelayProbe {
            delay,
            has_id,
            railed,
        }
    }

    #[test]
    fn railed_chip_id_delay_is_rejected() {
        // A delay whose register 63 matches but whose data is mostly railed must
        // not be accepted on chip-ID alone; a cleaner non-ID delay wins instead
        // of the railed ID delay (DA25).
        let probes = [probe(3, true, 0.95), probe(7, false, 0.10)];
        let (delay, by_id, railed) = choose_port_delay(&probes).unwrap();
        assert_eq!(delay, 7);
        assert!(!by_id);
        assert!((railed - 0.10).abs() < 1e-9);
    }

    #[test]
    fn clean_chip_id_delay_is_preferred_over_low_railed() {
        // With a chip-ID delay that also passes the railed gate, the strong
        // (by-id) path is taken even though a slightly cleaner non-id delay exists.
        let probes = [probe(4, true, 0.20), probe(9, false, 0.05)];
        let (delay, by_id, _) = choose_port_delay(&probes).unwrap();
        assert_eq!(delay, 4);
        assert!(by_id);
    }

    #[test]
    fn no_qualifying_delay_returns_none() {
        let probes = [probe(0, true, 0.99), probe(1, false, 0.80)];
        assert!(choose_port_delay(&probes).is_none());
    }

    #[test]
    fn second_good_delay_chosen_when_more_than_two() {
        // >2 good delays -> indexSecondGoodDelay (ascending order, index 1).
        let probes = [
            probe(4, true, 0.10),
            probe(5, true, 0.10),
            probe(6, true, 0.10),
            probe(7, true, 0.10),
        ];
        let (delay, by_id, _) = choose_port_delay(&probes).unwrap();
        assert_eq!(delay, 5);
        assert!(by_id);
    }

    #[test]
    fn chip_id_port_beats_fraction_only_port_regardless_of_quality() {
        // A fraction-only port with very clean data does NOT displace a
        // chip-ID-verified port (validation tier dominates) (DA24).
        assert!(!port_is_better(false, 0.01, true, 0.40));
        assert!(port_is_better(true, 0.40, false, 0.01));
    }

    #[test]
    fn cleaner_port_wins_within_same_tier_not_last_wins() {
        // Same validation tier: the lower railed fraction wins, so a later
        // (worse) reflection/cross-talk port can't overwrite a cleaner earlier
        // port by scan order (DA24).
        assert!(port_is_better(true, 0.05, true, 0.30));
        assert!(!port_is_better(true, 0.30, true, 0.05));
        assert!(!port_is_better(true, 0.05, true, 0.05));
    }
}
