//! Realistic in-vivo neural signal generator for demo mode.
//!
//! Generates multi-channel data that looks like real extracellular neural
//! recordings: background Gaussian noise, local field potential (LFP)
//! oscillations, and neural spike waveforms with Poisson timing.
//!
//! Each channel gets a random "personality":
//!   - quiet   : noise only, low amplitude
//!   - lfp     : prominent low-frequency oscillations
//!   - spiking : regular single-unit spikes
//!   - bursting: grouped spike bursts
//!   - noisy   : high noise floor with occasional spikes

use kv_types::SampleBlock;

/// Simple xorshift64 PRNG — separated from generator state to avoid
/// borrow-checker issues.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / ((1u64 << 53) as f64)
    }

    /// Approximate Gaussian via Central Limit (sum of 4 uniforms).
    fn gaussian(&mut self) -> f64 {
        let mut sum: f64 = 0.0;
        for _ in 0..4 {
            sum += self.next_f64();
        }
        (sum - 2.0) * 1.7
    }

    /// Exponential inter-spike interval in samples.
    fn poisson_interval(&mut self, rate_hz: f64, sample_rate: f64) -> u64 {
        if rate_hz <= 0.0 {
            return u64::MAX;
        }
        let u = self.next_f64().max(1e-10);
        let isi_seconds = -u.ln() / rate_hz;
        (isi_seconds * sample_rate).max(1.0) as u64
    }
}

/// Per-channel behaviour archetype.
#[derive(Debug, Clone, Copy)]
enum ChannelType {
    Quiet,
    Lfp,
    Spiking,
    Bursting,
    Noisy,
}

/// State for one channel's signal generator.
#[derive(Debug, Clone)]
struct ChannelGen {
    kind: ChannelType,
    noise_amplitude: f64,
    spike_rate_hz: f64,
    spike_amplitude: f64,
    lfp_freq_hz: f64,
    lfp_amplitude: f64,
    phase_offset: f64,
    next_spike_sample: u64,
    burst_remaining: u32,
    burst_isi_samples: u32,
}

/// Demo signal generator that produces SampleBlocks with neural-like data.
pub struct DemoGenerator {
    channels: Vec<ChannelGen>,
    channel_count: usize,
    sample_rate: f64,
    samples_per_packet: usize,
    packet_id: u64,
    global_sample: u64,
    rng: Rng,
}

impl DemoGenerator {
    pub fn new(channel_count: usize, sample_rate: f64, samples_per_packet: usize) -> Self {
        let mut rng = Rng::new(0xDEAD_BEEF_CAFE_1234);
        let mut channels = Vec::with_capacity(channel_count);

        for ch in 0..channel_count {
            channels.push(make_channel(ch, sample_rate, &mut rng));
        }

        Self {
            channels,
            channel_count,
            sample_rate,
            samples_per_packet,
            packet_id: 0,
            global_sample: 0,
            rng,
        }
    }

    /// Generate the next SampleBlock.
    pub fn next_block(&mut self) -> SampleBlock {
        let spc = self.samples_per_packet;
        let ch_count = self.channel_count;
        let sr = self.sample_rate;
        let mut data = vec![0i16; spc * ch_count];

        for s in 0..spc {
            let global_s = self.global_sample + s as u64;
            let t = global_s as f64 / sr;

            for ch in 0..ch_count {
                let value =
                    generate_sample(&mut self.channels[ch], &mut self.rng, global_s, t, ch, sr);
                data[s * ch_count + ch] = value;
            }
        }

        let timestamp_start = self.global_sample;
        // Saturate the running counters so an extremely long demo session can
        // never panic on overflow (debug) or silently wrap (release).
        self.global_sample = self.global_sample.saturating_add(spc as u64);
        let pid = self.packet_id;
        self.packet_id = self.packet_id.saturating_add(1);

        SampleBlock {
            device_id: "demo-neural".to_string(),
            stream_id: 0,
            packet_id: pid,
            timestamp_start,
            sample_rate: sr,
            channel_count: ch_count,
            samples_per_channel: spc,
            data,
            ttl_bits: 0,
            aux_data: None,
            board_adc_data: None,
            ttl_in_per_sample: None,
            ttl_out_per_sample: None,
        }
    }
}

fn make_channel(ch: usize, sample_rate: f64, rng: &mut Rng) -> ChannelGen {
    let r = rng.next_f64();
    let kind = match ch % 8 {
        0 => ChannelType::Spiking,
        1 => ChannelType::Lfp,
        2 => ChannelType::Bursting,
        3 => ChannelType::Spiking,
        4 => ChannelType::Quiet,
        5 => ChannelType::Noisy,
        6 => ChannelType::Lfp,
        7 => ChannelType::Spiking,
        _ => ChannelType::Quiet,
    };

    let (noise_amp, spike_rate, spike_amp, lfp_freq, lfp_amp) = match kind {
        ChannelType::Quiet => (80.0, 0.0, 0.0, 0.0, 0.0),
        ChannelType::Lfp => (100.0, 0.5, 600.0, 4.0 + r * 8.0, 800.0 + r * 600.0),
        ChannelType::Spiking => (120.0, 5.0 + r * 20.0, 800.0 + r * 1200.0, 2.0, 200.0),
        ChannelType::Bursting => (110.0, 8.0 + r * 15.0, 900.0 + r * 1000.0, 3.0, 300.0),
        ChannelType::Noisy => (250.0, 2.0, 500.0, 1.0, 150.0),
    };

    let next_spike = rng.poisson_interval(spike_rate, sample_rate);

    ChannelGen {
        kind,
        noise_amplitude: noise_amp,
        spike_rate_hz: spike_rate,
        spike_amplitude: spike_amp,
        lfp_freq_hz: lfp_freq,
        lfp_amplitude: lfp_amp,
        phase_offset: r * std::f64::consts::TAU,
        next_spike_sample: next_spike,
        burst_remaining: 0,
        burst_isi_samples: (sample_rate / 500.0).max(1.0) as u32,
    }
}

/// Generate one sample for one channel.  `cg` and `rng` are borrowed
/// independently so the borrow checker is happy.
fn generate_sample(
    cg: &mut ChannelGen,
    rng: &mut Rng,
    global_s: u64,
    t: f64,
    ch: usize,
    sample_rate: f64,
) -> i16 {
    let mut value: f64 = 0.0;

    // 1) Background noise
    value += rng.gaussian() * cg.noise_amplitude;

    // 2) LFP oscillation
    if cg.lfp_amplitude > 0.0 {
        let phase = std::f64::consts::TAU * cg.lfp_freq_hz * t + cg.phase_offset;
        value += phase.sin() * cg.lfp_amplitude;
        value += (phase * 2.3).sin() * cg.lfp_amplitude * 0.15;
    }

    // 3) Spike waveform — realistic 5-sample template (~0.17 ms at 30 kHz):
    //    s+0: small onset deflection
    //    s+1: large negative trough (action potential peak)
    //    s+2: positive overshoot (repolarization)
    //    s+3: after-hyperpolarization (AHP)
    //    s+4: recovery to baseline
    if cg.spike_rate_hz > 0.0
        && global_s >= cg.next_spike_sample
        && global_s < cg.next_spike_sample.saturating_add(5)
    {
        let spike_start = cg.next_spike_sample;
        let offset = global_s.saturating_sub(spike_start);
        let spike_val = match offset {
            0 => -0.3 * cg.spike_amplitude,  // onset
            1 => -cg.spike_amplitude,        // negative trough
            2 => 0.5 * cg.spike_amplitude,   // positive overshoot
            3 => -0.15 * cg.spike_amplitude, // AHP
            4 => 0.0,                        // recovery
            _ => 0.0,
        };
        value += spike_val;

        // Schedule next spike after the template completes
        if offset >= 4 {
            match cg.kind {
                ChannelType::Bursting => {
                    if cg.burst_remaining > 0 {
                        cg.burst_remaining -= 1;
                        cg.next_spike_sample = global_s + cg.burst_isi_samples as u64;
                    } else {
                        let burst_size = 3 + (rng.next_u64() % 4) as u32;
                        cg.burst_remaining = burst_size;
                        cg.next_spike_sample = global_s + cg.burst_isi_samples as u64;
                    }
                }
                _ => {
                    cg.next_spike_sample =
                        global_s + rng.poisson_interval(cg.spike_rate_hz, sample_rate);
                }
            }
        }
    }

    // 4) Slow baseline drift
    value += ((t * 0.05 + ch as f64 * 0.7).sin()) * 30.0;

    // Clamp to i16
    value.round().clamp(i16::MIN as f64, i16::MAX as f64) as i16
}

/// State for the demo mode preview.
pub struct DemoPreview {
    generator: DemoGenerator,
    pub sample_rate: f64,
    pub channel_count: usize,
    pub samples_per_packet: usize,
}

impl DemoPreview {
    pub fn new(channel_count: usize, sample_rate: f64, samples_per_packet: usize) -> Self {
        Self {
            generator: DemoGenerator::new(channel_count, sample_rate, samples_per_packet),
            sample_rate,
            channel_count,
            samples_per_packet,
        }
    }

    /// Default configuration matching typical neural recording settings.
    pub fn default_neural() -> Self {
        Self::new(32, 30000.0, 64)
    }

    /// Compute how many blocks should have been generated by `elapsed_seconds`.
    pub fn blocks_for_elapsed(&self, elapsed_seconds: f64) -> usize {
        if self.sample_rate <= 0.0 || self.samples_per_packet == 0 {
            return 0;
        }
        let seconds_per_block = self.samples_per_packet as f64 / self.sample_rate;
        (elapsed_seconds / seconds_per_block).floor() as usize
    }

    pub fn next_block(&mut self) -> SampleBlock {
        self.generator.next_block()
    }
}
