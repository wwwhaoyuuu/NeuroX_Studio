mod blanking;
mod envelope;
mod filter;
mod metrics;
mod threshold;
mod viewer;

pub use blanking::{ArtifactBlanker, BlankingConfig, BlankingReplacement};
pub use envelope::EnvelopeProcessor;
pub use filter::{BandPassProcessor, NotchProcessor};
pub use metrics::{MeasuredProcessor, MeasuredSink, NodeMetrics, NodeMetricsSnapshot};
pub use threshold::{
    ThresholdConfig, ThresholdDirection, ThresholdEventReceiver, ThresholdProcessor,
};
pub use viewer::{ViewerFrame, ViewerReceiver, ViewerSink, ViewerStats};

pub const ADC_MIDPOINT: f64 = 32_768.0;
