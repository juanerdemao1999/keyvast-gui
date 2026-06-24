//! Bounded sample block buffering for acquisition consumers.

use std::{collections::VecDeque, fmt, sync::Arc};

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

#[derive(Debug, Clone, Default)]
pub struct FanoutBlockBuffer {
    next_consumer_id: u64,
    consumers: Vec<ConsumerQueue>,
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

    /// Push a block to all consumers.  Returns `Some(PushOverflow)` if at
    /// least one consumer was full and had to evict its oldest block.
    pub fn push(&mut self, block: SampleBlock) -> Option<PushOverflow> {
        self.pushed_blocks = self.pushed_blocks.saturating_add(1);
        let block = Arc::new(block);

        let mut consumers_overflowed: usize = 0;
        let mut max_occupancy: f64 = 0.0;

        for consumer in &mut self.consumers {
            if consumer.push(Arc::clone(&block)) {
                consumers_overflowed += 1;
            }
            let occ = consumer.occupancy();
            if occ > max_occupancy {
                max_occupancy = occ;
            }
        }

        if consumers_overflowed > 0 {
            Some(PushOverflow {
                consumers_overflowed,
                total_dropped_blocks: self.consumers.iter().map(|c| c.dropped_blocks).sum(),
                max_occupancy,
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
        self.consumers
            .iter()
            .find(|consumer| consumer.id == consumer_id)
            .ok_or(BufferError::UnknownConsumer {
                id: consumer_id.as_u64(),
            })
    }

    fn consumer_mut(
        &mut self,
        consumer_id: BufferConsumerId,
    ) -> Result<&mut ConsumerQueue, BufferError> {
        self.consumers
            .iter_mut()
            .find(|consumer| consumer.id == consumer_id)
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

    fn occupancy(&self) -> f64 {
        if self.capacity_blocks == 0 {
            return 0.0;
        }
        self.blocks.len() as f64 / self.capacity_blocks as f64
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

/// Information returned when a [`FanoutBlockBuffer::push`] causes at least
/// one consumer to overflow (evict its oldest block).
#[derive(Debug, Clone, PartialEq)]
pub struct PushOverflow {
    /// How many consumers overflowed on this push.
    pub consumers_overflowed: usize,
    /// Cumulative dropped blocks across all consumers.
    pub total_dropped_blocks: u64,
    /// Highest occupancy among all consumers after the push.
    pub max_occupancy: f64,
}
