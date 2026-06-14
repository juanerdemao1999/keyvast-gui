# RHD Hardware Signal Debugging Log

Session date: 2026-06-14. Live hardware: Opal Kelly **XEM7310** (serial 2417001B8I) +
KeyVast headstage with an **Intan RHD2132 (32-channel)** chip, on Windows.

This file records everything established while debugging "the acquired signal is
wrong". It is meant to be a durable reference so we don't re-derive it.

---

## 1. System context

- **Data plane** = the stock Intan "Rhythm" USB3 engine (`module main`,
  `keyvast-fpga-demo/hw/rhythm/rtl/main.v`). Board ID = 700. SPI to RHD over the
  module ring; data streamed to PC over FrontPanel USB3 (Opal Kelly).
- **Control plane** = a MicroBlaze (`keyvast_top.sv`) that, *after* FPGA
  configuration, runs an **I²C power-up sequence** for the headstage
  (`sw/src/power_seq.c`: eFuse → per-module DCDC → per-module **isolated power**)
  and blinks the user LEDs as a heartbeat. Isolated power (what actually powers
  the RHD) comes up ~0.6 s after config and is otherwise retried on a ~6 s cadence
  (`main.c`: `REVERIFY_TICKS=50 × 120 ms`). The external power chips (TCAL9538)
  **latch** their enable state across FPGA reconfigs.
- **Host** = `kv-rhd` crate (Rust port of the Open Ephys "RHD Rec Controller"
  rhythm-api). Entry: `RhdHardwareBackend::open` → `RhythmFrontPanelBoard::configure`.
- **Open Ephys reference** = `external/rhd-recording-controller` (the exact plugin
  the user runs). For XEM7310 it uploads `intan_rec_controller_7310.bit`
  (`DeviceThread.cpp:357`). Open Ephys reads this same hardware **cleanly** — it is
  the ground truth.

### Bitfiles (project root)
| file | sha256[0:16] | size | notes |
|---|---|---|---|
| intan_rec_controller_7310.bit | (Jun 11, 2,605,421 B) | 2.6 MB | **the real Open Ephys bitfile** (user copied it over). USE THIS. |
| keyvast_combined_download.bit | 62E0E9C0B084929C | 2,435,857 B | KeyVast combined image (MicroBlaze + Rhythm) |
| keyvast_260607_with_UART.bit | 2B96F01513CEB4B5 | 2,522,101 B | KeyVast w/ UART |
| (old) intan_rec_controller_7310.bit | 9BBA1975…| 2,435,857 B | a DIFFERENT/older build — earlier tests used this by mistake |

Note: any of these brings up board_id=700 and the RHD2132 responds (the headstage
power latches across reconfigs).

---

## 2. Recording / data format facts

- `recording.kvraw` (kv-rhd, via GUI) = `KEYVAST\n` magic + 4-byte LE json-len +
  512-byte JSON header, **data starts at byte 524**, then i16-LE interleaved
  *signed counts* = `raw_word − 32768`. (`kv-recorder/src/lib.rs`, KVRAW v2.)
- CLI `kv-acq rhd-smoke` writes v1 (`recording.json` external, data from byte 0).
- Open Ephys binary: `continuous.dat` = i16 interleaved, 0.195 µV/bit; metadata in
  `structure.oebin`.
- Scale: `0.195 µV/count`, midscale `0x8000`. Confirmed identical in both stacks.

---

## 3. Bugs found AND FIXED (validated on hardware)

### 3a. Half-scale 0x4000 baseline + "no chip / flat data" on cold start
**Root cause:** `RhythmFrontPanelBoard::configure` ran `reset_board()` + the
port/MISO-delay scan **immediately** after `ConfigureFPGA`, *before* the headstage
I²C rails were up. The scan saw idle MISO (0xFFFF), found no chip, and silently
fell back to **Port A / delay 0** — a MISO phase one SPI bit early, which
right-shifts the 16-bit amplifier word → baseline **0x4000** (half scale) and
half-amplitude signal. (A 2nd reconfig "worked" only because the rails were already
latched.)

**Fix (`kv-rhd/src/backend.rs`):** after `ConfigureFPGA`, sleep
`HEADSTAGE_POWER_SETTLE_MS` (1200 ms), then **retry the whole scan** up to
`SCAN_MAX_ATTEMPTS` (6) × `SCAN_RETRY_MS` (600 ms) until a chip answers.
`scan_ports_for_headstage` now returns a `found` flag.

**Verified:** cold power-cycle → chip located on scan attempt 1, chip-ID-verified
delays 4–7, baseline back to **~0x8000**.

### 3b. Chip auto-detect
`RhdChipType::from_register63` was wrong (it bit-shifted). Real reg-63 = literal
Intan chip ID (**1=RHD2132/32ch, 2=RHD2216/16ch, 4=RHD2164/64ch**, matching Open
Ephys `getDeviceId`). Fixed; wired into the scan so the backend auto-sets the data-
stream count / channel count from the detected chip (overrides the requested value).

**Verified:** `--streams 2` is auto-overridden to 1 stream / 32 ch; recording
metadata `channel_count=32`. Raw AuxCmd3 readback literally spells `RHD2132`+`INTAN`.

### 3c. Engineering
- `kv-cli` (`kv-acq`) now initializes `env_logger` (`RUST_LOG`, default info).
- Scan logs per-delay `has_id / chip_id / railed / amp_mean_raw` for responding
  delays (helpers `amplifier_mean_raw_word`, `probe_chip_id`).
- Tests pass (`cargo test --workspace --exclude kv-gui`); `kv-rhd` clippy clean.

---

## 4. The "16 real + 16 phantom" pattern is NOT a bug — it is the hardware

Cross-correlation of kv-rhd data: the 16 **odd-index** channels (kvraw idx
1,3,5,…,31) are near-identical to each other (mean |r| = 0.999); the 16 **even-
index** channels are distinct real signals.

**Open Ephys's OWN recording has the identical structure**: CH1,3,5,…,31 (idx
0,2,4…) are distinct real; CH2,4,6,…,32 (idx 1,3,5…) are all the same ~87 µV
(mean |r| = 0.999). → Half the headstage channels are connected to electrodes, half
float (showing common noise). kv-rhd reproduces this exactly. **Do not chase this.**

---

## 5. The remaining open issue: kv-rhd is noisier than Open Ephys (5–300 Hz / mains)

Direct comparison vs the Open Ephys recording
(`openephys_data/2026-06-14_23-06-20/...`), per-band RMS (phantom ch1, FFT):

| band | kv-rhd (delay 6) | Open Ephys | verdict |
|---|---|---|---|
| 0–5 Hz (drift) | 17 | 18 µV | match (DSP HPF works in both) |
| 5–50 Hz | 226 | 64 µV | kv ~3.5× |
| **45–65 Hz (mains)** | **271** | **33 µV** | kv ~8× |
| 50–300 Hz | 239 | 33 µV | kv ~7× |
| 300–3000 Hz (spikes) | 17 | 5 µV | |
| 3000–9000 Hz (floor) | 2.4 | 2.0 µV | match |

So the excess is **only in the 5–300 Hz analog band, peaking at 50/60 Hz mains**.
DC, the spike band, and the noise floor all match → rules out digital / MISO bit
errors / clock / sample-rate problems (mains sits at 50/60 Hz, i.e. the 30 kHz
rate is correct, not aliased). User confirms the **environment is identical**, so
this is a real kv-rhd vs Open Ephys difference, not ambient pickup.

### Things CONFIRMED to MATCH Open Ephys (ruled out)
- Bitfile (`intan_rec_controller_7310.bit`), board_id 700.
- Chip RAM registers 0–17, read back FROM the chip: reg0=0xde, reg1→adcBufferBias=2,
  reg2→muxBias=4, reg4→weakMiso=1/dsp_en=1/dsp_cutoff=12, amps all powered (reg14-17=0xff).
- Sample-rate PLL M/D for 30 kHz = **(42, 25)** in both.
- defineSampleRate muxBias/adcBufferBias for 30 kHz.
- Command-list structure/order (createCommandListRegisterConfig etc.).

### Differences still NOT reconciled
- **MISO cable delay**: Open Ephys uses `optimumDelay = indexSecondGoodDelay`
  (= delay **5** for good delays 4–7). kv-rhd's heuristic chose delay **6**.
  Forcing delay 5 dropped real-ch0 noise a lot (5–50 Hz 101→34 µV; mains 124→41 µV),
  but a 3→8 delay sweep was **inconsistent** because the mains pickup itself
  fluctuates between captures (delay 3 once read ~50 µV, delays 4–7 ~118 µV, delay
  8 railed). So delay matters, but it doesn't fully close the gap to Open Ephys (~18 µV).
- **Upper bandwidth**: kv-rhd uses 10 000 Hz; Open Ephys `HighCut=7500`. (Affects
  >300 Hz only; not the 5–300 Hz excess. Worth aligning for true 1:1.)
- **Headstage power path**: Open Ephys runs with rails brought up by the KeyVast
  MicroBlaze (keyvast bit) then the stock bit; kv-rhd relies on latched rails. A
  marginal/noisier isolated-power rail could degrade amplifier CMRR → mains. Unverified.

---

## 6. Leading hypotheses for the 5–300 Hz / mains excess (to test next)
1. **MISO delay selection** — match Open Ephys exactly: pick `indexSecondGoodDelay`
   among *chip-ID-valid* delays (not the broad "INTAN present" set / middle).
   Possibly pick the delay that **minimizes in-band amplifier RMS**.
2. **Power quality** — ensure the headstage isolated rail is fully/cleanly up
   (run/clock the MicroBlaze power sequence, or verify rail voltage) before acquiring.
3. **Bandwidth 1:1** — set amplifier upper bandwidth to 7500 Hz (match `HighCut`).
4. **Back-to-back capture** — record kv-rhd and Open Ephys within the same minute to
   factor out any residual ambient variation when comparing.

---

## 7. Diagnostic tooling created (project root, scratch — not committed)
- `analyze_kvraw.py` — per-channel stats of a `recording.kvraw`.
- `corr.py` — cross-correlation (proves the even/odd phantom).
- `decode_raw.py` / `decode2.py` — decode a raw Rhythm wire block (`raw_block.bin`):
  AuxCmd register readback (chip ID) + amplifier even/odd.
- `bandmeasure.py` + `sweep_delays.ps1` — per-band (mains) RMS vs MISO delay sweep.
- Temporary in-code DIAG hooks were used (raw-block dump, `diag_delay_sweep`,
  forced sample rate, `KVDELAY` env override). **These must be reverted**; only the
  power-settle/retry + auto-detect + logging changes are meant to stay.

---

## 8. Status summary
- FIXED & verified: cold-start power timing, 0x4000→0x8000 baseline, 32-ch auto-detect.
- NOT a bug: the 16-real/16-phantom channel split (hardware; Open Ephys shows it too).
- STILL OPEN: kv-rhd has ~5–8× more 5–300 Hz / 50–60 Hz mains noise than Open Ephys
  on identical hardware+environment. Most promising lever so far: the MISO cable-delay
  selection (Open Ephys = indexSecondGoodDelay = 5; kv-rhd picked 6). Needs a cleaner,
  fluctuation-robust delay-selection + likely a power-rail check.
