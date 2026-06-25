//! Bounded sample block buffering for acquisition consumers.

use std::{
    collections::{HashMap, VecDeque},
    fmt,
    sync::Arc,
};

use kv_types::SampleBlock;

/// Information about a block drop that occurred during a push.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OverflowInfo {
    pub dropped_blocks: u64,
    pub buffer_occupancy: f64,
}

#[derive(Debug, Clone)]
pub struct BlockBuffer {
    capacity_blocks: usize,
    blocks: VecDeque<SampleBlock>,
    pushed_blocks: u64,
    dropped_blocks: u64,
}

impl BlockBuffer {
    pub fn new(capacity_blocks: usize) -> Result<Self, BufferError> {
        if capacity_blocks == 0 {
            return Err(BufferError::ZeroCapacity);
        }

        Ok(Self {
            capacity_blocks,
            blocks: VecDeque::with_capacity(capacity_blocks),
            pushed_blocks: 0,
            dropped_blocks: 0,
        })
    }

    pub fn push(&mut self, block: SampleBlock) -> Option<OverflowInfo> {
        self.pushed_blocks = self.pushed_blocks.saturating_add(1);

        let overflowed = if self.blocks.len() == self.capacity_blocks {
            self.blocks.pop_front();
            self.dropped_blocks = self.dropped_blocks.saturating_add(1);
            true
        } else {
            false
        };

        self.blocks.push_back(block);

        if overflowed {
            Some(OverflowInfo {
                dropped_blocks: self.dropped_blocks,
                buffer_occupancy: self.blocks.len() as f64 / self.capacity_blocks as f64,
            })
        } else {
            None
        }
    }

    pub fn pop(&mut self) -> Option<SampleBlock> {
        self.blocks.pop_front()
    }

    pub fn len(&self) -> usize {
        self.blocks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    pub fn capacity_blocks(&self) -> usize {
        self.capacity_blocks
    }

    pub fn status(&self) -> BufferStatus {
        BufferStatus {
            capacity_blocks: self.capacity_blocks,
            buffered_blocks: self.blocks.len(),
            pushed_blocks: self.pushed_blocks,
            dropped_blocks: self.dropped_blocks,
            occupancy: self.blocks.len() as f64 / self.capacity_blocks as f64,
        }
    }
}

/// A block buffer that fans every pushed block out to several independent,
/// bounded consumer queues (e.g. a recorder and a GUI preview), each draining
/// at its own pace.
#[derive(Debug, Clone, Default)]
pub struct FanoutBlockBuffer {
    next_consumer_id: u64,
    consumers: Vec<ConsumerQueue>,
    /// Maps a consumer id to its index in `consumers`, so per-consumer lookups
    /// stay O(1) instead of linearly scanning while holding the producer lock.
    index: HashMap<BufferConsumerId, usize>,
    pushed_blocks: u64,
}

impl FanoutBlockBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_consumer(
        &mut self,
        name: impl Into<String>,
        capacity_blocks: usize,
    ) -> Result<BufferConsumerId, BufferError> {
        if capacity_blocks == 0 {
            return Err(BufferError::ZeroCapacity);
        }

        let id = BufferConsumerId(self.next_consumer_id);
        self.next_consumer_id = self.next_consumer_id.saturating_add(1);
        self.index.insert(id, self.consumers.len());
        self.consumers.push(ConsumerQueue {
            id,
            name: name.into(),
            capacity_blocks,
            blocks: VecDeque::with_capacity(capacity_blocks),
            pushed_blocks: 0,
            dropped_blocks: 0,
            popped_blocks: 0,
        });

        Ok(id)
    }

    /// Push a block to all consumers. Returns overflow info for any consumer
    /// that dropped a block, along with the total dropped blocks across all
    /// consumers.
    pub fn push(&mut self, block: SampleBlock) -> Option<OverflowInfo> {
        self.push_arc(Arc::new(block))
    }

    /// Push an already-`Arc`-wrapped block to all consumers.
    ///
    /// Lets the producer share a single allocation between the GUI preview
    /// channel and the fanout buffer, avoiding a deep copy of the block on the
    /// real-time acquisition thread.
    pub fn push_arc(&mut self, block: Arc<SampleBlock>) -> Option<OverflowInfo> {
        self.pushed_blocks = self.pushed_blocks.saturating_add(1);

        let mut total_dropped: u64 = 0;
        let mut any_overflow = false;
        for consumer in &mut self.consumers {
            if consumer.push(Arc::clone(&block)) {
                any_overflow = true;
            }
            total_dropped = total_dropped.saturating_add(consumer.dropped_blocks);
        }

        if any_overflow {
            let max_occupancy = self
                .consumers
                .iter()
                .map(|c| c.blocks.len() as f64 / c.capacity_blocks as f64)
                .fold(0.0_f64, f64::max);
            Some(OverflowInfo {
                dropped_blocks: total_dropped,
                buffer_occupancy: max_occupancy,
            })
        } else {
            None
        }
    }

    pub fn pop(
        &mut self,
        consumer_id: BufferConsumerId,
    ) -> Result<Option<Arc<SampleBlock>>, BufferError> {
        let consumer = self.consumer_mut(consumer_id)?;
        let block = consumer.blocks.pop_front();

        if block.is_some() {
            consumer.popped_blocks = consumer.popped_blocks.saturating_add(1);
        }

        Ok(block)
    }

    pub fn status(&self) -> FanoutBufferStatus {
        FanoutBufferStatus {
            consumer_count: self.consumers.len(),
            pushed_blocks: self.pushed_blocks,
        }
    }

    pub fn consumer_status(
        &self,
        consumer_id: BufferConsumerId,
    ) -> Result<ConsumerBufferStatus, BufferError> {
        Ok(self.consumer(consumer_id)?.status())
    }

    pub fn consumer_statuses(&self) -> Vec<ConsumerBufferStatus> {
        self.consumers.iter().map(ConsumerQueue::status).collect()
    }

    fn consumer(&self, consumer_id: BufferConsumerId) -> Result<&ConsumerQueue, BufferError> {
        self.index
            .get(&consumer_id)
            .and_then(|&idx| self.consumers.get(idx))
            .ok_or(BufferError::UnknownConsumer {
                id: consumer_id.as_u64(),
            })
    }

    fn consumer_mut(
        &mut self,
        consumer_id: BufferConsumerId,
    ) -> Result<&mut ConsumerQueue, BufferError> {
        let idx = *self
            .index
            .get(&consumer_id)
            .ok_or(BufferError::UnknownConsumer {
                id: consumer_id.as_u64(),
            })?;
        self.consumers
            .get_mut(idx)
            .ok_or(BufferError::UnknownConsumer {
                id: consumer_id.as_u64(),
            })
    }
}

#[derive(Debug, Clone)]
struct ConsumerQueue {
    id: BufferConsumerId,
    name: String,
    capacity_blocks: usize,
    blocks: VecDeque<Arc<SampleBlock>>,
    pushed_blocks: u64,
    dropped_blocks: u64,
    popped_blocks: u64,
}

impl ConsumerQueue {
    /// Push a block. Returns `true` if a block was dropped due to overflow.
    fn push(&mut self, block: Arc<SampleBlock>) -> bool {
        self.pushed_blocks = self.pushed_blocks.saturating_add(1);

        let overflowed = if self.blocks.len() == self.capacity_blocks {
            self.blocks.pop_front();
            self.dropped_blocks = self.dropped_blocks.saturating_add(1);
            true
        } else {
            false
        };

        self.blocks.push_back(block);
        overflowed
    }

    fn status(&self) -> ConsumerBufferStatus {
        ConsumerBufferStatus {
            consumer_id: self.id,
            name: self.name.clone(),
            capacity_blocks: self.capacity_blocks,
            buffered_blocks: self.blocks.len(),
            pushed_blocks: self.pushed_blocks,
            popped_blocks: self.popped_blocks,
            dropped_blocks: self.dropped_blocks,
            occupancy: self.blocks.len() as f64 / self.capacity_blocks as f64,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BufferConsumerId(u64);

impl BufferConsumerId {
    pub fn as_u64(self) -> u64 {
        self.0
    }
}

impl fmt::Display for BufferConsumerId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BufferStatus {
    pub capacity_blocks: usize,
    pub buffered_blocks: usize,
    pub pushed_blocks: u64,
    pub dropped_blocks: u64,
    pub occupancy: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FanoutBufferStatus {
    pub consumer_count: usize,
    pub pushed_blocks: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConsumerBufferStatus {
    pub consumer_id: BufferConsumerId,
    pub name: String,
    pub capacity_blocks: usize,
    pub buffered_blocks: usize,
    pub pushed_blocks: u64,
    pub popped_blocks: u64,
    pub dropped_blocks: u64,
    pub occupancy: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BufferError {
    ZeroCapacity,
    UnknownConsumer { id: u64 },
}

impl fmt::Display for BufferError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroCapacity => write!(formatter, "buffer capacity must be greater than zero"),
            Self::UnknownConsumer { id } => {
                write!(formatter, "buffer consumer {id} does not exist")
            }
        }
    }
}

impl std::error::Error for BufferError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_block(packet_id: u64) -> SampleBlock {
        SampleBlock {
            device_id: "test".to_string(),
            stream_id: 0,
            packet_id,
            timestamp_start: packet_id,
            sample_rate: 30_000.0,
            channel_count: 1,
            samples_per_channel: 1,
            ttl_bits: 0,
            data: vec![0],
            aux_data: None,
            board_adc_data: None,
            ttl_in_per_sample: None,
            ttl_out_per_sample: None,
        }
    }

    #[test]
    fn consumer_queue_drops_oldest_block_on_overflow() {
        let mut queue = ConsumerQueue {
            id: BufferConsumerId(0),
            name: "c".to_string(),
            capacity_blocks: 2,
            blocks: VecDeque::new(),
            pushed_blocks: 0,
            dropped_blocks: 0,
            popped_blocks: 0,
        };

        assert!(!queue.push(Arc::new(test_block(0))));
        assert!(!queue.push(Arc::new(test_block(1))));
        // Third push exceeds capacity and evicts packet 0.
        assert!(queue.push(Arc::new(test_block(2))));
        assert_eq!(queue.dropped_blocks, 1);
        assert_eq!(queue.pushed_blocks, 3);
        assert_eq!(
            queue.blocks.front().map(|block| block.packet_id),
            Some(1),
            "oldest block should have been evicted"
        );
    }

    #[test]
    fn pop_and_status_reject_unknown_consumer() {
        let mut buffer = FanoutBlockBuffer::new();
        let bogus = BufferConsumerId(99);

        assert_eq!(
            buffer.pop(bogus),
            Err(BufferError::UnknownConsumer { id: 99 })
        );
        assert_eq!(
            buffer.consumer_status(bogus),
            Err(BufferError::UnknownConsumer { id: 99 })
        );
    }

    #[test]
    fn fanout_lookup_is_stable_across_multiple_consumers() {
        let mut buffer = FanoutBlockBuffer::new();
        let recorder = buffer.add_consumer("recorder", 4).expect("valid capacity");
        let preview = buffer.add_consumer("preview", 1).expect("valid capacity");

        buffer.push(test_block(0));
        buffer.push(test_block(1));

        // The small preview queue overflows while the recorder retains both.
        assert_eq!(
            buffer
                .consumer_status(recorder)
                .expect("known")
                .buffered_blocks,
            2
        );
        let preview_status = buffer.consumer_status(preview).expect("known");
        assert_eq!(preview_status.buffered_blocks, 1);
        assert_eq!(preview_status.dropped_blocks, 1);

        assert_eq!(
            buffer.pop(recorder).expect("known").map(|b| b.packet_id),
            Some(0)
        );
    }

    #[test]
    fn add_consumer_rejects_zero_capacity() {
        let mut buffer = FanoutBlockBuffer::new();
        assert_eq!(
            buffer.add_consumer("zero", 0),
            Err(BufferError::ZeroCapacity)
        );
    }
}
