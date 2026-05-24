//! Background preview source that runs a simulator and sends SampleBlocks
//! to the GUI thread via an mpsc channel.

use std::sync::mpsc;
use std::thread;

use kv_simulator::{SimulatorBackend, SimulatorConfig};
use kv_types::SampleBlock;

/// Commands sent from the GUI to the preview thread.
pub enum PreviewCommand {
    Stop,
}

/// Handle returned by `start_preview` for communicating with the background thread.
pub struct PreviewHandle {
    pub receiver: mpsc::Receiver<SampleBlock>,
    pub command_sender: mpsc::Sender<PreviewCommand>,
    join_handle: Option<thread::JoinHandle<()>>,
}

impl PreviewHandle {
    /// Drain all available blocks from the channel, returning the most recent one.
    pub fn latest_block(&self) -> Option<SampleBlock> {
        let mut latest = None;
        while let Ok(block) = self.receiver.try_recv() {
            latest = Some(block);
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

/// Start a background thread that continuously generates SampleBlocks from
/// the simulator and sends them through a channel. The thread respects a
/// simulated packet rate so blocks arrive at approximately real-time cadence.
pub fn start_preview(config: SimulatorConfig) -> PreviewHandle {
    let (block_sender, block_receiver) = mpsc::channel();
    let (cmd_sender, cmd_receiver) = mpsc::channel();

    let sample_rate = config.device.sample_rate;
    let samples_per_packet = config.device.samples_per_packet;

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

        loop {
            if let Ok(PreviewCommand::Stop) = cmd_receiver.try_recv() {
                break;
            }

            match simulator.next_block() {
                Ok(block) => {
                    if block_sender.send(block).is_err() {
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
