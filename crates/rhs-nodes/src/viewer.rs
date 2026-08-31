use rhs_core::{ChannelId, SampleBlock, SampleFlags};
use rhs_pipeline::{PipelineError, Sink};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewerFrame {
    pub source_id: String,
    pub sequence: u64,
    pub first_sample_index: u64,
    pub sample_rate_hz: u32,
    pub bin_width_samples: u32,
    pub point_count: u32,
    pub channels: Vec<ChannelId>,
    pub minimums: Vec<u16>,
    pub maximums: Vec<u16>,
    pub flags: SampleFlags,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ViewerStats {
    pub enqueued: u64,
    pub received: u64,
    pub dropped_oldest: u64,
    pub current_depth: u64,
    pub max_depth: u64,
    pub total_processing_ns: u64,
    pub max_processing_ns: u64,
    pub last_queue_latency_ns: u64,
    pub max_queue_latency_ns: u64,
}

struct QueuedFrame {
    frame: ViewerFrame,
    queued_at: Instant,
}

struct ViewerShared {
    queue: Mutex<VecDeque<QueuedFrame>>,
    capacity: usize,
    finished: AtomicBool,
    enqueued: AtomicU64,
    received: AtomicU64,
    dropped_oldest: AtomicU64,
    current_depth: AtomicU64,
    max_depth: AtomicU64,
    total_processing_ns: AtomicU64,
    max_processing_ns: AtomicU64,
    last_queue_latency_ns: AtomicU64,
    max_queue_latency_ns: AtomicU64,
}

impl ViewerShared {
    fn stats(&self) -> ViewerStats {
        ViewerStats {
            enqueued: self.enqueued.load(Ordering::Relaxed),
            received: self.received.load(Ordering::Relaxed),
            dropped_oldest: self.dropped_oldest.load(Ordering::Relaxed),
            current_depth: self.current_depth.load(Ordering::Relaxed),
            max_depth: self.max_depth.load(Ordering::Relaxed),
            total_processing_ns: self.total_processing_ns.load(Ordering::Relaxed),
            max_processing_ns: self.max_processing_ns.load(Ordering::Relaxed),
            last_queue_latency_ns: self.last_queue_latency_ns.load(Ordering::Relaxed),
            max_queue_latency_ns: self.max_queue_latency_ns.load(Ordering::Relaxed),
        }
    }
}

pub struct ViewerSink {
    shared: Arc<ViewerShared>,
    max_points_per_block: usize,
}

pub struct ViewerReceiver {
    shared: Arc<ViewerShared>,
}

impl ViewerSink {
    pub fn new(
        queue_capacity: usize,
        max_points_per_block: usize,
    ) -> Result<(Self, ViewerReceiver), PipelineError> {
        if queue_capacity == 0 {
            return Err(PipelineError::new(
                "viewer",
                "queue_capacity must be greater than zero",
            ));
        }
        if max_points_per_block == 0 {
            return Err(PipelineError::new(
                "viewer",
                "max_points_per_block must be greater than zero",
            ));
        }
        let shared = Arc::new(ViewerShared {
            queue: Mutex::new(VecDeque::with_capacity(queue_capacity)),
            capacity: queue_capacity,
            finished: AtomicBool::new(false),
            enqueued: AtomicU64::new(0),
            received: AtomicU64::new(0),
            dropped_oldest: AtomicU64::new(0),
            current_depth: AtomicU64::new(0),
            max_depth: AtomicU64::new(0),
            total_processing_ns: AtomicU64::new(0),
            max_processing_ns: AtomicU64::new(0),
            last_queue_latency_ns: AtomicU64::new(0),
            max_queue_latency_ns: AtomicU64::new(0),
        });
        Ok((
            Self {
                shared: Arc::clone(&shared),
                max_points_per_block,
            },
            ViewerReceiver { shared },
        ))
    }
}

impl Sink for ViewerSink {
    fn name(&self) -> &str {
        "viewer"
    }

    fn consume(&mut self, block: &SampleBlock) -> Result<(), PipelineError> {
        let started = Instant::now();
        block
            .validate()
            .map_err(|error| PipelineError::new("viewer", error.to_string()))?;
        let frame = build_frame(block, self.max_points_per_block);
        let mut queue = self
            .shared
            .queue
            .lock()
            .map_err(|_| PipelineError::new("viewer", "viewer queue lock was poisoned"))?;
        if queue.len() == self.shared.capacity {
            queue.pop_front();
            self.shared.dropped_oldest.fetch_add(1, Ordering::Relaxed);
        }
        queue.push_back(QueuedFrame {
            frame,
            queued_at: Instant::now(),
        });
        let depth = queue.len() as u64;
        self.shared.enqueued.fetch_add(1, Ordering::Relaxed);
        self.shared.current_depth.store(depth, Ordering::Relaxed);
        self.shared.max_depth.fetch_max(depth, Ordering::Relaxed);
        drop(queue);

        let elapsed_ns = started.elapsed().as_nanos().min(u64::MAX as u128) as u64;
        self.shared
            .total_processing_ns
            .fetch_add(elapsed_ns, Ordering::Relaxed);
        self.shared
            .max_processing_ns
            .fetch_max(elapsed_ns, Ordering::Relaxed);
        Ok(())
    }

    fn finish(&mut self) -> Result<(), PipelineError> {
        self.shared.finished.store(true, Ordering::Release);
        Ok(())
    }
}

impl ViewerReceiver {
    pub fn try_recv(&self) -> Option<ViewerFrame> {
        let queued = self.shared.queue.lock().ok()?.pop_front()?;
        self.shared.received.fetch_add(1, Ordering::Relaxed);
        self.shared.current_depth.fetch_sub(1, Ordering::Relaxed);
        let latency_ns = queued.queued_at.elapsed().as_nanos().min(u64::MAX as u128) as u64;
        self.shared
            .last_queue_latency_ns
            .store(latency_ns, Ordering::Relaxed);
        self.shared
            .max_queue_latency_ns
            .fetch_max(latency_ns, Ordering::Relaxed);
        Some(queued.frame)
    }

    pub fn drain_latest(&self) -> Option<ViewerFrame> {
        let mut latest = None;
        while let Some(frame) = self.try_recv() {
            latest = Some(frame);
        }
        latest
    }

    pub fn stats(&self) -> ViewerStats {
        self.shared.stats()
    }

    pub fn is_finished(&self) -> bool {
        self.shared.finished.load(Ordering::Acquire)
    }
}

fn build_frame(block: &SampleBlock, max_points: usize) -> ViewerFrame {
    let input_points = block.samples_per_channel as usize;
    let bin_width = input_points.div_ceil(max_points).max(1);
    let point_count = input_points.div_ceil(bin_width);
    let channel_count = block.channel_count();
    let mut minimums = Vec::with_capacity(point_count * channel_count);
    let mut maximums = Vec::with_capacity(point_count * channel_count);
    for point in 0..point_count {
        let first = point * bin_width;
        let end = (first + bin_width).min(input_points);
        for channel in 0..channel_count {
            let mut minimum = u16::MAX;
            let mut maximum = u16::MIN;
            for sample in first..end {
                let value = block.samples[sample * channel_count + channel];
                minimum = minimum.min(value);
                maximum = maximum.max(value);
            }
            minimums.push(minimum);
            maximums.push(maximum);
        }
    }
    ViewerFrame {
        source_id: block.source_id.clone(),
        sequence: block.sequence,
        first_sample_index: block.first_sample_index,
        sample_rate_hz: block.sample_rate_hz,
        bin_width_samples: bin_width as u32,
        point_count: point_count as u32,
        channels: block.channels.clone(),
        minimums,
        maximums,
        flags: block.flags,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rhs_core::{ChannelId, SampleFlags};
    use rhs_pipeline::{
        CancellationToken, PipelineRunner, Processor, SyntheticConfig, SyntheticSource,
    };
    use std::sync::atomic::AtomicU64;

    fn block(sequence: u64, samples: Vec<u16>) -> SampleBlock {
        SampleBlock::new(
            "test",
            sequence,
            sequence * samples.len() as u64,
            30_000,
            vec![ChannelId {
                stream: 0,
                channel: 0,
            }],
            samples.len() as u32,
            samples,
            Vec::new(),
            SampleFlags::empty(),
        )
        .unwrap()
    }

    #[test]
    fn creates_min_max_display_envelope() {
        let (mut sink, receiver) = ViewerSink::new(2, 2).unwrap();
        sink.consume(&block(0, vec![1, 4, 2, 3, 8, 5, 7, 6]))
            .unwrap();
        let frame = receiver.try_recv().unwrap();

        assert_eq!(frame.bin_width_samples, 4);
        assert_eq!(frame.point_count, 2);
        assert_eq!(frame.minimums, vec![1, 5]);
        assert_eq!(frame.maximums, vec![4, 8]);
    }

    #[test]
    fn slow_viewer_drops_old_frames_without_stopping_other_sinks() {
        struct CountingSink(Arc<AtomicU64>);

        impl Sink for CountingSink {
            fn name(&self) -> &str {
                "counting"
            }

            fn consume(&mut self, _block: &SampleBlock) -> Result<(), PipelineError> {
                self.0.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }

            fn finish(&mut self) -> Result<(), PipelineError> {
                Ok(())
            }
        }

        let (viewer, receiver) = ViewerSink::new(2, 16).unwrap();
        let received_by_other_sink = Arc::new(AtomicU64::new(0));
        let counting = CountingSink(Arc::clone(&received_by_other_sink));
        let mut source = SyntheticSource::new(SyntheticConfig {
            total_blocks: Some(10),
            ..SyntheticConfig::default()
        })
        .unwrap();
        let mut processors: Vec<Box<dyn Processor>> = Vec::new();
        let mut sinks: Vec<Box<dyn Sink>> = vec![Box::new(viewer), Box::new(counting)];
        PipelineRunner::new(CancellationToken::new())
            .run(&mut source, &mut processors, &mut sinks)
            .unwrap();

        assert_eq!(received_by_other_sink.load(Ordering::Relaxed), 10);
        assert_eq!(receiver.stats().dropped_oldest, 8);
        assert_eq!(receiver.stats().current_depth, 2);
        assert_eq!(receiver.try_recv().unwrap().sequence, 8);
        assert_eq!(receiver.try_recv().unwrap().sequence, 9);
        assert!(receiver.is_finished());
    }
}
