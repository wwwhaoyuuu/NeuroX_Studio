use rhs_core::{Event, EventKind, SampleBlock, SampleFlags};
use rhs_pipeline::{PipelineError, Processor};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, TryRecvError, TrySendError};
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThresholdDirection {
    RisingAbove,
    FallingBelow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThresholdConfig {
    pub channel_index: usize,
    pub threshold: u16,
    pub direction: ThresholdDirection,
    pub refractory_samples: u32,
    pub event_queue_capacity: usize,
}

pub struct ThresholdEventReceiver {
    receiver: Receiver<Event>,
    generated: Arc<AtomicU64>,
}

impl ThresholdEventReceiver {
    pub fn try_recv(&self) -> Result<Event, TryRecvError> {
        self.receiver.try_recv()
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Result<Event, RecvTimeoutError> {
        self.receiver.recv_timeout(timeout)
    }

    pub fn generated(&self) -> u64 {
        self.generated.load(Ordering::Relaxed)
    }
}

pub struct ThresholdProcessor {
    config: ThresholdConfig,
    sender: mpsc::SyncSender<Event>,
    generated: Arc<AtomicU64>,
    previous: Option<u16>,
    next_allowed_sample: u64,
    event_sequence: u64,
}

impl ThresholdProcessor {
    pub fn new(config: ThresholdConfig) -> Result<(Self, ThresholdEventReceiver), PipelineError> {
        if config.event_queue_capacity == 0 {
            return Err(PipelineError::new(
                "threshold",
                "event_queue_capacity must be greater than zero",
            ));
        }
        let (sender, receiver) = mpsc::sync_channel(config.event_queue_capacity);
        let generated = Arc::new(AtomicU64::new(0));
        Ok((
            Self {
                config,
                sender,
                generated: Arc::clone(&generated),
                previous: None,
                next_allowed_sample: 0,
                event_sequence: 0,
            },
            ThresholdEventReceiver {
                receiver,
                generated,
            },
        ))
    }
}

impl Processor for ThresholdProcessor {
    fn name(&self) -> &str {
        "threshold"
    }

    fn process(&mut self, block: SampleBlock) -> Result<Option<SampleBlock>, PipelineError> {
        block
            .validate()
            .map_err(|error| PipelineError::new("threshold", error.to_string()))?;
        if self.config.channel_index >= block.channel_count() {
            return Err(PipelineError::new(
                "threshold",
                format!(
                    "channel index {} is outside {} channels",
                    self.config.channel_index,
                    block.channel_count()
                ),
            ));
        }
        if block.flags.contains(SampleFlags::DISCONTINUITY) {
            self.previous = None;
            self.next_allowed_sample = block.first_sample_index;
        }

        let channel_count = block.channel_count();
        for sample_offset in 0..block.samples_per_channel as usize {
            let current = block.samples[sample_offset * channel_count + self.config.channel_index];
            let sample_index = block
                .first_sample_index
                .saturating_add(sample_offset as u64);
            let crossed = self
                .previous
                .is_some_and(|previous| match self.config.direction {
                    ThresholdDirection::RisingAbove => {
                        previous < self.config.threshold && current >= self.config.threshold
                    }
                    ThresholdDirection::FallingBelow => {
                        previous > self.config.threshold && current <= self.config.threshold
                    }
                });
            if crossed && sample_index >= self.next_allowed_sample {
                let event = Event {
                    sequence: self.event_sequence,
                    sample_index: Some(sample_index),
                    kind: EventKind::ThresholdCrossing,
                    message: format!(
                        "channel={} threshold={} value={} direction={:?}",
                        self.config.channel_index,
                        self.config.threshold,
                        current,
                        self.config.direction
                    ),
                };
                match self.sender.try_send(event) {
                    Ok(()) => {
                        self.event_sequence = self.event_sequence.saturating_add(1);
                        self.generated.fetch_add(1, Ordering::Relaxed);
                        self.next_allowed_sample =
                            sample_index.saturating_add(self.config.refractory_samples as u64);
                    }
                    Err(TrySendError::Full(_)) => {
                        return Err(PipelineError::new(
                            "threshold",
                            "threshold event queue is full; refusing to lose an event silently",
                        ));
                    }
                    Err(TrySendError::Disconnected(_)) => {
                        return Err(PipelineError::new(
                            "threshold",
                            "threshold event receiver disconnected",
                        ));
                    }
                }
            }
            self.previous = Some(current);
        }
        Ok(Some(block))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rhs_core::{ChannelId, SampleFlags};

    fn block(samples: Vec<u16>) -> SampleBlock {
        SampleBlock::new(
            "test",
            0,
            100,
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
    fn emits_crossings_with_absolute_sample_indices() {
        let (mut processor, receiver) = ThresholdProcessor::new(ThresholdConfig {
            channel_index: 0,
            threshold: 200,
            direction: ThresholdDirection::RisingAbove,
            refractory_samples: 2,
            event_queue_capacity: 4,
        })
        .unwrap();
        processor
            .process(block(vec![100, 200, 250, 100, 210]))
            .unwrap();

        let first = receiver.try_recv().unwrap();
        let second = receiver.try_recv().unwrap();
        assert_eq!(first.sample_index, Some(101));
        assert_eq!(second.sample_index, Some(104));
        assert_eq!(first.kind, EventKind::ThresholdCrossing);
        assert_eq!(receiver.generated(), 2);
    }

    #[test]
    fn full_event_queue_is_an_explicit_pipeline_error() {
        let (mut processor, receiver) = ThresholdProcessor::new(ThresholdConfig {
            channel_index: 0,
            threshold: 200,
            direction: ThresholdDirection::RisingAbove,
            refractory_samples: 0,
            event_queue_capacity: 1,
        })
        .unwrap();
        let error = processor
            .process(block(vec![100, 210, 100, 220]))
            .unwrap_err();

        assert!(error.to_string().contains("queue is full"));
        assert!(receiver.try_recv().is_ok());
        assert_eq!(receiver.generated(), 1);
    }
}
