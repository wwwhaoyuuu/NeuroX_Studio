use rhs_core::SampleBlock;
use rhs_pipeline::{PipelineError, Processor, Sink};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NodeMetricsSnapshot {
    pub calls: u64,
    pub errors: u64,
    pub total_processing_ns: u64,
    pub max_processing_ns: u64,
    pub last_processing_ns: u64,
}

#[derive(Debug, Clone, Default)]
pub struct NodeMetrics {
    inner: Arc<NodeMetricsInner>,
}

#[derive(Debug, Default)]
struct NodeMetricsInner {
    calls: AtomicU64,
    errors: AtomicU64,
    total_processing_ns: AtomicU64,
    max_processing_ns: AtomicU64,
    last_processing_ns: AtomicU64,
}

impl NodeMetrics {
    pub fn snapshot(&self) -> NodeMetricsSnapshot {
        NodeMetricsSnapshot {
            calls: self.inner.calls.load(Ordering::Relaxed),
            errors: self.inner.errors.load(Ordering::Relaxed),
            total_processing_ns: self.inner.total_processing_ns.load(Ordering::Relaxed),
            max_processing_ns: self.inner.max_processing_ns.load(Ordering::Relaxed),
            last_processing_ns: self.inner.last_processing_ns.load(Ordering::Relaxed),
        }
    }

    fn record(&self, elapsed_ns: u64, failed: bool) {
        self.inner.calls.fetch_add(1, Ordering::Relaxed);
        if failed {
            self.inner.errors.fetch_add(1, Ordering::Relaxed);
        }
        self.inner
            .total_processing_ns
            .fetch_add(elapsed_ns, Ordering::Relaxed);
        self.inner
            .max_processing_ns
            .fetch_max(elapsed_ns, Ordering::Relaxed);
        self.inner
            .last_processing_ns
            .store(elapsed_ns, Ordering::Relaxed);
    }
}

pub struct MeasuredProcessor<P> {
    inner: P,
    name: String,
    metrics: NodeMetrics,
}

impl<P: Processor> MeasuredProcessor<P> {
    pub fn new(inner: P) -> (Self, NodeMetrics) {
        let metrics = NodeMetrics::default();
        let measured = Self {
            name: inner.name().to_owned(),
            inner,
            metrics: metrics.clone(),
        };
        (measured, metrics)
    }

    pub fn inner(&self) -> &P {
        &self.inner
    }

    pub fn inner_mut(&mut self) -> &mut P {
        &mut self.inner
    }
}

impl<P: Processor> Processor for MeasuredProcessor<P> {
    fn name(&self) -> &str {
        &self.name
    }

    fn process(&mut self, block: SampleBlock) -> Result<Option<SampleBlock>, PipelineError> {
        let started = Instant::now();
        let result = self.inner.process(block);
        self.metrics.record(
            started.elapsed().as_nanos().min(u64::MAX as u128) as u64,
            result.is_err(),
        );
        result
    }
}

pub struct MeasuredSink<S> {
    inner: S,
    name: String,
    metrics: NodeMetrics,
}

impl<S: Sink> MeasuredSink<S> {
    pub fn new(inner: S) -> (Self, NodeMetrics) {
        let metrics = NodeMetrics::default();
        let measured = Self {
            name: inner.name().to_owned(),
            inner,
            metrics: metrics.clone(),
        };
        (measured, metrics)
    }

    pub fn inner(&self) -> &S {
        &self.inner
    }
}

impl<S: Sink> Sink for MeasuredSink<S> {
    fn name(&self) -> &str {
        &self.name
    }

    fn consume(&mut self, block: &SampleBlock) -> Result<(), PipelineError> {
        let started = Instant::now();
        let result = self.inner.consume(block);
        self.metrics.record(
            started.elapsed().as_nanos().min(u64::MAX as u128) as u64,
            result.is_err(),
        );
        result
    }

    fn finish(&mut self) -> Result<(), PipelineError> {
        self.inner.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rhs_pipeline::PassThrough;

    fn block() -> SampleBlock {
        SampleBlock::new(
            "test",
            0,
            0,
            30_000,
            vec![rhs_core::ChannelId {
                stream: 0,
                channel: 0,
            }],
            1,
            vec![32_768],
            Vec::new(),
            rhs_core::SampleFlags::empty(),
        )
        .unwrap()
    }

    #[test]
    fn measured_processor_counts_calls() {
        let (mut processor, metrics) = MeasuredProcessor::new(PassThrough);
        processor.process(block()).unwrap();
        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.calls, 1);
        assert_eq!(snapshot.errors, 0);
        assert!(snapshot.max_processing_ns >= snapshot.last_processing_ns);
    }
}
