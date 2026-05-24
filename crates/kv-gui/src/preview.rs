//! Background preview source that runs a simulator and sends SampleBlocks
//! to the GUI thread via an mpsc channel, along with real-time statistics.

use std::collections::VecDeque;
use std::sync::mpsc;
use std::thread;
use std::time::Instant;

use kv_simulator::{SimulatorBackend, SimulatorConfig};
use kv_types::SampleBlock;

/// Commands sent from the GUI to the preview thread.
pub enum PreviewCommand {
    Stop,
}

/// Per-channel real-time statistics.
#[derive(Debug, Clone)]
pub struct ChannelStats {
    pub rms: f64,
    pub peak_to_peak: i16,
    pub min: i16,
    pub max: i16,
}

/// Aggregated statistics for the latest block.
#[derive(Debug, Clone)]
pub struct BlockStats {
    pub channels: Vec<ChannelStats>,
    pub data_rate_mb_s: f64,
    pub block_rate_hz: f64,
    pub total_blocks: u64,
    pub total_samples: u64,
    pub elapsed_seconds: f64,
    pub dropped_blocks: u64,
}

/// Handle returned by `start_preview` for communicating with the background thread.
pub struct PreviewHandle {
    pub receiver: mpsc::Receiver<(SampleBlock, BlockStats)>,
    pub command_sender: mpsc::Sender<PreviewCommand>,
    join_handle: Option<thread::JoinHandle<()>>,
}

impl PreviewHandle {
    /// Drain all available blocks from the channel, returning the most recent one.
    pub fn latest(&self) -> Option<(SampleBlock, BlockStats)> {
        let mut latest = None;
        while let Ok(item) = self.receiver.try_recv() {
            latest = Some(item);
        }
        latest
    }

    /// Send a stop command to the preview thread.
    pub fn stop(&self) {
        let _ = self.command_sender.send(PreviewCommand::Stop);
    }
}

impl Drop for PreviewHandle {
    fn drop(&mut self) {
        self.stop();
        if let Some(handle) = self.join_handle.take() {
            let _ = handle.join();
        }
    }
}

/// Compute BlockStats from a single block (used by demo mode).
pub fn compute_block_stats(
    block: &SampleBlock,
    total_blocks: u64,
    elapsed_seconds: f64,
) -> BlockStats {
    let channel_stats = compute_channel_stats(block);
    let total_samples = total_blocks * (block.samples_per_channel * block.channel_count) as u64;
    let bytes_total = total_samples * 2;
    let data_rate_mb_s = if elapsed_seconds > 0.0 {
        bytes_total as f64 / elapsed_seconds / 1_000_000.0
    } else {
        0.0
    };
    let block_rate_hz = if elapsed_seconds > 0.0 {
        total_blocks as f64 / elapsed_seconds
    } else {
        0.0
    };
    BlockStats {
        channels: channel_stats,
        data_rate_mb_s,
        block_rate_hz,
        total_blocks,
        total_samples,
        elapsed_seconds,
        dropped_blocks: 0,
    }
}

/// Compute per-channel statistics from a SampleBlock.
fn compute_channel_stats(block: &SampleBlock) -> Vec<ChannelStats> {
    let ch_count = block.channel_count;
    let spc = block.samples_per_channel;
    if ch_count == 0 || spc == 0 {
        return Vec::new();
    }

    (0..ch_count)
        .map(|ch| {
            let mut sum_sq: f64 = 0.0;
            let mut min_val = i16::MAX;
            let mut max_val = i16::MIN;

            for s in 0..spc {
                let idx = s * ch_count + ch;
                let v = if idx < block.data.len() {
                    block.data[idx]
                } else {
                    0
                };
                sum_sq += (v as f64) * (v as f64);
                min_val = min_val.min(v);
                max_val = max_val.max(v);
            }

            ChannelStats {
                rms: (sum_sq / spc as f64).sqrt(),
                peak_to_peak: max_val.saturating_sub(min_val),
                min: min_val,
                max: max_val,
            }
        })
        .collect()
}

/// Start a background thread that continuously generates SampleBlocks from
/// the simulator and sends them with computed statistics.
pub fn start_preview(config: SimulatorConfig) -> PreviewHandle {
    let (block_sender, block_receiver) = mpsc::channel();
    let (cmd_sender, cmd_receiver) = mpsc::channel();

    let sample_rate = config.device.sample_rate;
    let samples_per_packet = config.device.samples_per_packet;
    let _channel_count = config.device.channel_count;

    let join_handle = thread::spawn(move || {
        let Ok(mut simulator) = SimulatorBackend::new(config) else {
            return;
        };

        let seconds_per_block = if sample_rate > 0.0 && samples_per_packet > 0 {
            samples_per_packet as f64 / sample_rate
        } else {
            0.001
        };
        let sleep_duration = std::time::Duration::from_secs_f64(seconds_per_block.max(0.000_001));

        let start_time = Instant::now();
        let mut total_blocks: u64 = 0;
        let mut total_samples: u64 = 0;

        // Sliding window for block rate measurement
        let mut block_times: VecDeque<Instant> = VecDeque::with_capacity(128);

        loop {
            if let Ok(PreviewCommand::Stop) = cmd_receiver.try_recv() {
                break;
            }

            match simulator.next_block() {
                Ok(block) => {
                    let now = Instant::now();
                    total_blocks += 1;
                    total_samples += block.data.len() as u64;

                    block_times.push_back(now);
                    // Keep only last 1 second of timestamps
                    while let Some(&front) = block_times.front() {
                        if now.duration_since(front).as_secs_f64() > 1.0 {
                            block_times.pop_front();
                        } else {
                            break;
                        }
                    }

                    let elapsed = now.duration_since(start_time).as_secs_f64();
                    let bytes_total = total_samples * 2; // i16 = 2 bytes
                    let data_rate_mb_s = if elapsed > 0.0 {
                        bytes_total as f64 / elapsed / 1_000_000.0
                    } else {
                        0.0
                    };

                    let block_rate_hz = if block_times.len() > 1 {
                        let window = now
                            .duration_since(*block_times.front().unwrap())
                            .as_secs_f64();
                        if window > 0.0 {
                            (block_times.len() - 1) as f64 / window
                        } else {
                            0.0
                        }
                    } else {
                        0.0
                    };

                    let channel_stats = compute_channel_stats(&block);

                    let stats = BlockStats {
                        channels: channel_stats,
                        data_rate_mb_s,
                        block_rate_hz,
                        total_blocks,
                        total_samples,
                        elapsed_seconds: elapsed,
                        dropped_blocks: 0,
                    };

                    if block_sender.send((block, stats)).is_err() {
                        break;
                    }
                }
                Err(_) => {
                    break;
                }
            }

            thread::sleep(sleep_duration);
        }
    });

    PreviewHandle {
        receiver: block_receiver,
        command_sender: cmd_sender,
        join_handle: Some(join_handle),
    }
}

/// Application-side state that keeps a scrolling history of recent blocks.
pub struct PreviewState {
    pub handle: Option<PreviewHandle>,
    pub latest_block: Option<SampleBlock>,
    pub latest_stats: Option<BlockStats>,
    pub block_history: VecDeque<SampleBlock>,
    pub history_capacity: usize,
    pub acquiring: bool,
}

impl PreviewState {
    pub fn new() -> Self {
        Self {
            handle: None,
            latest_block: None,
            latest_stats: None,
            block_history: VecDeque::with_capacity(64),
            history_capacity: 64,
            acquiring: false,
        }
    }

    pub fn start(&mut self, config: SimulatorConfig) {
        if self.acquiring {
            return;
        }
        self.handle = Some(start_preview(config));
        self.acquiring = true;
        self.latest_block = None;
        self.latest_stats = None;
        self.block_history.clear();
    }

    pub fn stop(&mut self) {
        if let Some(ref handle) = self.handle {
            handle.stop();
        }
        self.handle = None;
        self.acquiring = false;
    }

    /// Poll the preview channel for new data.  Returns true if new data arrived.
    pub fn poll(&mut self) -> bool {
        let Some(ref handle) = self.handle else {
            return false;
        };
        let Some((block, stats)) = handle.latest() else {
            return false;
        };

        self.block_history.push_back(block.clone());
        while self.block_history.len() > self.history_capacity {
            self.block_history.pop_front();
        }

        self.latest_block = Some(block);
        self.latest_stats = Some(stats);
        true
    }
}
