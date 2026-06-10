use kv_buffer::{BlockBuffer, BufferError, FanoutBlockBuffer};
use kv_simulator::SimulatorBackend;
use kv_types::SampleBlock;

#[test]
fn buffer_pops_blocks_in_fifo_order() {
    let blocks = simulator_blocks(2);
    let mut buffer = BlockBuffer::new(2).expect("capacity is valid");

    buffer.push(blocks[0].clone());
    buffer.push(blocks[1].clone());

    assert_eq!(buffer.len(), 2);
    assert_eq!(buffer.pop().map(|block| block.packet_id), Some(0));
    assert_eq!(buffer.pop().map(|block| block.packet_id), Some(1));
    assert_eq!(buffer.pop(), None);
    assert_eq!(buffer.status().dropped_blocks, 0);
}

#[test]
fn full_buffer_drops_oldest_block_and_counts_overflow() {
    let blocks = simulator_blocks(3);
    let mut buffer = BlockBuffer::new(2).expect("capacity is valid");

    for block in blocks {
        buffer.push(block);
    }

    assert_eq!(buffer.len(), 2);
    assert_eq!(buffer.status().dropped_blocks, 1);
    assert_eq!(buffer.pop().map(|block| block.packet_id), Some(1));
    assert_eq!(buffer.pop().map(|block| block.packet_id), Some(2));
}

#[test]
fn status_tracks_occupancy_and_push_counters() {
    let blocks = simulator_blocks(2);
    let mut buffer = BlockBuffer::new(4).expect("capacity is valid");

    buffer.push(blocks[0].clone());
    buffer.push(blocks[1].clone());

    let status = buffer.status();
    assert_eq!(status.capacity_blocks, 4);
    assert_eq!(status.buffered_blocks, 2);
    assert_eq!(status.pushed_blocks, 2);
    assert_eq!(status.dropped_blocks, 0);
    assert_eq!(status.occupancy, 0.5);
}

#[test]
fn zero_capacity_is_rejected() {
    assert_eq!(BlockBuffer::new(0).unwrap_err(), BufferError::ZeroCapacity);
}

#[test]
fn fanout_consumers_advance_independently() {
    let blocks = simulator_blocks(2);
    let mut buffer = FanoutBlockBuffer::new();
    let recorder = buffer
        .add_consumer("recorder", 4)
        .expect("capacity is valid");
    let preview = buffer
        .add_consumer("preview", 4)
        .expect("capacity is valid");

    buffer.push(blocks[0].clone());
    buffer.push(blocks[1].clone());

    assert_eq!(
        buffer.pop(recorder).unwrap().map(|block| block.packet_id),
        Some(0)
    );

    let recorder_status = buffer.consumer_status(recorder).unwrap();
    let preview_status = buffer.consumer_status(preview).unwrap();
    assert_eq!(recorder_status.buffered_blocks, 1);
    assert_eq!(preview_status.buffered_blocks, 2);

    assert_eq!(
        buffer.pop(preview).unwrap().map(|block| block.packet_id),
        Some(0)
    );
    assert_eq!(
        buffer.pop(preview).unwrap().map(|block| block.packet_id),
        Some(1)
    );
    assert_eq!(
        buffer.pop(recorder).unwrap().map(|block| block.packet_id),
        Some(1)
    );
}

#[test]
fn slow_preview_drops_old_blocks_without_affecting_recorder() {
    let blocks = simulator_blocks(4);
    let mut buffer = FanoutBlockBuffer::new();
    let recorder = buffer
        .add_consumer("recorder", 4)
        .expect("capacity is valid");
    let preview = buffer
        .add_consumer("preview", 2)
        .expect("capacity is valid");

    for block in blocks {
        buffer.push(block);
    }

    let recorder_status = buffer.consumer_status(recorder).unwrap();
    let preview_status = buffer.consumer_status(preview).unwrap();
    assert_eq!(recorder_status.dropped_blocks, 0);
    assert_eq!(recorder_status.buffered_blocks, 4);
    assert_eq!(preview_status.dropped_blocks, 2);
    assert_eq!(preview_status.buffered_blocks, 2);

    let recorder_packets = drain_packet_ids(&mut buffer, recorder);
    let preview_packets = drain_packet_ids(&mut buffer, preview);
    assert_eq!(recorder_packets, vec![0, 1, 2, 3]);
    assert_eq!(preview_packets, vec![2, 3]);
}

#[test]
fn late_fanout_consumer_starts_from_future_blocks_only() {
    let blocks = simulator_blocks(3);
    let mut buffer = FanoutBlockBuffer::new();
    let recorder = buffer
        .add_consumer("recorder", 4)
        .expect("capacity is valid");

    buffer.push(blocks[0].clone());
    let preview = buffer
        .add_consumer("preview", 4)
        .expect("capacity is valid");
    buffer.push(blocks[1].clone());
    buffer.push(blocks[2].clone());

    let preview_status = buffer.consumer_status(preview).unwrap();
    assert_eq!(preview_status.pushed_blocks, 2);
    assert_eq!(preview_status.buffered_blocks, 2);

    assert_eq!(drain_packet_ids(&mut buffer, recorder), vec![0, 1, 2]);
    assert_eq!(drain_packet_ids(&mut buffer, preview), vec![1, 2]);
}

#[test]
fn fanout_rejects_zero_capacity_consumer() {
    let mut buffer = FanoutBlockBuffer::new();

    assert_eq!(
        buffer.add_consumer("preview", 0).unwrap_err(),
        BufferError::ZeroCapacity
    );
}

fn drain_packet_ids(
    buffer: &mut FanoutBlockBuffer,
    consumer: kv_buffer::BufferConsumerId,
) -> Vec<u64> {
    let mut packet_ids = Vec::new();

    while let Some(block) = buffer.pop(consumer).expect("consumer exists") {
        packet_ids.push(block.packet_id);
    }

    packet_ids
}

fn simulator_blocks(count: usize) -> Vec<SampleBlock> {
    let mut simulator = SimulatorBackend::default();

    (0..count)
        .map(|_| {
            simulator
                .next_block()
                .expect("default simulator should emit blocks")
        })
        .collect()
}
