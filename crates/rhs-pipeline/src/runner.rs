use crate::Source;
use rhs_core::SampleBlock;
use std::error::Error as StdError;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub trait Processor {
    fn name(&self) -> &str;
    fn process(&mut self, block: SampleBlock) -> Result<Option<SampleBlock>, PipelineError>;
}

pub trait Sink {
    fn name(&self) -> &str;
    fn consume(&mut self, block: &SampleBlock) -> Result<(), PipelineError>;
    fn finish(&mut self) -> Result<(), PipelineError>;
}

#[derive(Debug, Default)]
pub struct PassThrough;

impl Processor for PassThrough {
    fn name(&self) -> &str {
        "pass-through"
    }

    fn process(&mut self, block: SampleBlock) -> Result<Option<SampleBlock>, PipelineError> {
        Ok(Some(block))
    }
}

#[derive(Debug, Clone, Default)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl CancellationToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PipelineRunReport {
    pub source_blocks: u64,
    pub emitted_blocks: u64,
    pub dropped_blocks: u64,
    pub cancelled: bool,
}

pub struct PipelineRunner {
    cancellation: CancellationToken,
}

impl PipelineRunner {
    pub fn new(cancellation: CancellationToken) -> Self {
        Self { cancellation }
    }

    pub fn cancellation(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    pub fn run<S: Source>(
        &self,
        source: &mut S,
        processors: &mut [Box<dyn Processor>],
        sinks: &mut [Box<dyn Sink>],
    ) -> Result<PipelineRunReport, PipelineError> {
        let mut report = PipelineRunReport::default();

        while !self.cancellation.is_cancelled() {
            let Some(block) = source
                .next_block()
                .map_err(|error| PipelineError::new("source", error.to_string()))?
            else {
                break;
            };
            report.source_blocks += 1;

            let mut current = Some(block);
            for processor in processors.iter_mut() {
                let processor_name = processor.name().to_string();
                let input = current
                    .take()
                    .expect("processor input must exist before processing");
                current = processor.process(input).map_err(|error| {
                    PipelineError::new(format!("processor:{processor_name}"), error.to_string())
                })?;
                if current.is_none() {
                    break;
                }
            }

            let Some(block) = current else {
                report.dropped_blocks += 1;
                continue;
            };

            for sink in sinks.iter_mut() {
                let sink_name = sink.name().to_string();
                sink.consume(&block).map_err(|error| {
                    PipelineError::new(format!("sink:{sink_name}"), error.to_string())
                })?;
            }
            report.emitted_blocks += 1;
        }

        report.cancelled = self.cancellation.is_cancelled();
        for sink in sinks.iter_mut() {
            let sink_name = sink.name().to_string();
            sink.finish().map_err(|error| {
                PipelineError::new(format!("sink:{sink_name}:finish"), error.to_string())
            })?;
        }
        Ok(report)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipelineError {
    pub stage: String,
    pub message: String,
}

impl PipelineError {
    pub fn new(stage: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            stage: stage.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for PipelineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.stage, self.message)
    }
}

impl StdError for PipelineError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SyntheticConfig, SyntheticSource};

    #[derive(Default)]
    struct CountingSink {
        blocks: u64,
        finished: bool,
    }

    impl Sink for CountingSink {
        fn name(&self) -> &str {
            "counting"
        }

        fn consume(&mut self, _block: &SampleBlock) -> Result<(), PipelineError> {
            self.blocks += 1;
            Ok(())
        }

        fn finish(&mut self) -> Result<(), PipelineError> {
            self.finished = true;
            Ok(())
        }
    }

    struct DropOddSequence;

    impl Processor for DropOddSequence {
        fn name(&self) -> &str {
            "drop-odd"
        }

        fn process(&mut self, block: SampleBlock) -> Result<Option<SampleBlock>, PipelineError> {
            Ok(block.sequence.is_multiple_of(2).then_some(block))
        }
    }

    #[test]
    fn runs_source_processors_and_sink_to_completion() {
        let mut source = SyntheticSource::new(SyntheticConfig {
            total_blocks: Some(10),
            ..SyntheticConfig::default()
        })
        .unwrap();
        let mut processors: Vec<Box<dyn Processor>> = vec![Box::new(DropOddSequence)];
        let mut sinks: Vec<Box<dyn Sink>> = vec![Box::new(CountingSink::default())];
        let runner = PipelineRunner::new(CancellationToken::new());

        let report = runner
            .run(&mut source, &mut processors, &mut sinks)
            .unwrap();

        assert_eq!(report.source_blocks, 10);
        assert_eq!(report.emitted_blocks, 5);
        assert_eq!(report.dropped_blocks, 5);
        assert!(!report.cancelled);
    }

    #[test]
    fn cancellation_finishes_sinks_without_reading_source() {
        let mut source = SyntheticSource::new(SyntheticConfig {
            total_blocks: Some(10),
            ..SyntheticConfig::default()
        })
        .unwrap();
        let mut processors: Vec<Box<dyn Processor>> = vec![Box::new(PassThrough)];
        let mut sinks: Vec<Box<dyn Sink>> = vec![Box::new(CountingSink::default())];
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let runner = PipelineRunner::new(cancellation);

        let report = runner
            .run(&mut source, &mut processors, &mut sinks)
            .unwrap();

        assert_eq!(report.source_blocks, 0);
        assert!(report.cancelled);
    }
}
