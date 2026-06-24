//! Bounded sample block buffering for acquisition consumers.

use std::{
    collections::{HashMap, VecDeque},
    fmt,
    sync::Arc,
};

use kv_types::SampleBlock;

/// Single-consumer bounded ring buffer of [`SampleBlock`]s.
#[derive(Debug, Clone)]
pub struct BlockBuffer {
    capacity_blocks: usize,
    blocks: VecDeque<SampleBlock>,
    pushed_blocks: u64,
    dropped_blocks: u64,
}

impl BlockBuffer {
    /// Create a new buffer with the given maximum capacity (in blocks).
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

    /// Push a block into the buffer.  Returns `true` if the buffer was full
    /// and the oldest block was evicted (overflow).
    pub fn push(&mut self, block: SampleBlock) -> bool {
        self.pushed_blocks = self.pushed_blocks.saturating_add(1);

        let overflowed = self.blocks.len() == self.capacity_blocks;
        if overflowed {
            self.blocks.pop_front();
            self.dropped_blocks = self.dropped_blocks.saturating_add(1);
        }

        self.blocks.push_back(block);
        overflowed
    }

    /// Remove and return the oldest block, or `None` if the buffer is empty.
    pub fn pop(&mut self) -> Option<SampleBlock> {
        self.blocks.pop_front()
    }

    /// Number of blocks currently buffered.
    pub fn len(&self) -> usize {
        self.blocks.len()
    }

    /// Returns `true` if the buffer contains no blocks.
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    /// Maximum number of blocks the buffer can hold before evicting.
    pub fn capacity_blocks(&self) -> usize {
        self.capacity_blocks
    }

    /// Snapshot of occupancy and throughput counters.
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

/// Multi-consumer fanout buffer: each pushed block is replicated to all
/// registered consumers via `Arc` sharing, with independent per-consumer
/// ring overflow semantics.
#[derive(Debug, Clone, Default)]
pub struct FanoutBlockBuffer {
    next_consumer_id: u64,
    consumers: HashMap<BufferConsumerId, ConsumerQueue>,
    pushed_blocks: u64,
}

impl FanoutBlockBuffer {
    /// Create an empty fanout buffer with no consumers.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a new consumer with the given name and per-consumer capacity.
    /// Returns the consumer's unique ID for subsequent `pop` / `status` calls.
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
        self.consumers.insert(
            id,
            ConsumerQueue {
                id,
                name: name.into(),
                capacity_blocks,
                blocks: VecDeque::with_capacity(capacity_blocks),
                pushed_blocks: 0,
                dropped_blocks: 0,
                popped_blocks: 0,
            },
        );

        Ok(id)
    }

    /// Push a block to all consumers.  Returns `Some(PushOverflow)` if at
    /// least one consumer was full and had to evict its oldest block.
    pub fn push(&mut self, block: SampleBlock) -> Option<PushOverflow> {
        self.push_arc(Arc::new(block))
    }

    /// Like [`push`](Self::push), but accepts a pre-wrapped `Arc<SampleBlock>`.
    /// Use this when the Arc allocation should happen outside a critical
    /// section (e.g. before acquiring a Mutex).
    pub fn push_arc(&mut self, block: Arc<SampleBlock>) -> Option<PushOverflow> {
        self.pushed_blocks = self.pushed_blocks.saturating_add(1);

        let mut consumers_overflowed: usize = 0;
        let mut max_occupancy: f64 = 0.0;

        for consumer in self.consumers.values_mut() {
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
                total_dropped_blocks: self.consumers.values().map(|c| c.dropped_blocks).sum(),
                max_occupancy,
            })
        } else {
            None
        }
    }

    /// Pop the oldest block for a specific consumer, or `None` if that
    /// consumer's queue is empty.  Returns `Err` if the ID is unknown.
    pub fn pop(
        &mut self,
        consumer_id: BufferConsumerId,
    ) -> Result<Option<Arc<SampleBlock>>, BufferError> {
        let consumer =
            self.consumers
                .get_mut(&consumer_id)
                .ok_or(BufferError::UnknownConsumer {
                    id: consumer_id.as_u64(),
                })?;
        let block = consumer.blocks.pop_front();

        if block.is_some() {
            consumer.popped_blocks = consumer.popped_blocks.saturating_add(1);
        }

        Ok(block)
    }

    /// Aggregate buffer status (consumer count and total pushes).
    pub fn status(&self) -> FanoutBufferStatus {
        FanoutBufferStatus {
            consumer_count: self.consumers.len(),
            pushed_blocks: self.pushed_blocks,
        }
    }

    /// Per-consumer status snapshot.  Returns `Err` if the ID is unknown.
    pub fn consumer_status(
        &self,
        consumer_id: BufferConsumerId,
    ) -> Result<ConsumerBufferStatus, BufferError> {
        self.consumers
            .get(&consumer_id)
            .map(|c| c.status())
            .ok_or(BufferError::UnknownConsumer {
                id: consumer_id.as_u64(),
            })
    }

    /// Status snapshots for all registered consumers.
    pub fn consumer_statuses(&self) -> Vec<ConsumerBufferStatus> {
        self.consumers.values().map(ConsumerQueue::status).collect()
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
    fn push(&mut self, block: Arc<SampleBlock>) -> bool {
        self.pushed_blocks = self.pushed_blocks.saturating_add(1);

        let overflowed = self.blocks.len() == self.capacity_blocks;
        if overflowed {
            self.blocks.pop_front();
            self.dropped_blocks = self.dropped_blocks.saturating_add(1);
        }

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

/// Opaque handle identifying a registered consumer within a [`FanoutBlockBuffer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BufferConsumerId(u64);

impl BufferConsumerId {
    /// Return the underlying numeric ID.
    pub fn as_u64(self) -> u64 {
        self.0
    }

    /// Construct an ID from a raw u64.  Intended for testing error paths
    /// (e.g. verifying `UnknownConsumer` handling).
    pub fn from_raw(raw: u64) -> Self {
        Self(raw)
    }
}

impl fmt::Display for BufferConsumerId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

/// Snapshot of a [`BlockBuffer`]'s occupancy and throughput counters.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BufferStatus {
    pub capacity_blocks: usize,
    pub buffered_blocks: usize,
    pub pushed_blocks: u64,
    pub dropped_blocks: u64,
    pub occupancy: f64,
}

/// Aggregate status of a [`FanoutBlockBuffer`] (consumer count + total pushes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FanoutBufferStatus {
    pub consumer_count: usize,
    pub pushed_blocks: u64,
}

/// Per-consumer occupancy and throughput snapshot within a [`FanoutBlockBuffer`].
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

/// Errors produced by buffer operations (invalid capacity, unknown consumer).
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
