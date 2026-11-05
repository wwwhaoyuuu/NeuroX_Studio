# Spike detection and recording

This is threshold-based candidate spike detection, not spike sorting or a
validated identification of neuronal firing. Open inputs, clipping, mains noise
and unmarked stimulation artifacts can all create false detections.

## Operation

1. Stop acquisition (or close playback). Open the Spike tab.
2. Enable multichannel detection. Select the signed threshold in uV, refractory
   period, pre/post windows, and optional dedicated 300-3000 Hz band-pass.
3. For a durable event record, enable continuous CSV and choose a **new** filename.
   Existing files are never overwritten. Apply, then start acquisition or playback.
4. Switch between raster, channel waveforms and event values. Stop acquisition or
   close playback to finalize the CSV. Pausing playback does not finalize it.

The default -50 uV threshold is an editable starting value, not an experimentally
validated threshold for a given preparation. Negative thresholds detect downward
crossings; positive thresholds detect upward crossings. Refractory duration must
be at least the post-window. Changes take effect in the next session.

## Data contract

- Detection consumes full-rate sample blocks **after the ordered processing
  pipeline**, before display decimation. The optional dedicated band-pass is an
  additional filter, so enabling both it and a pipeline band-pass cascades them.
- Display gain and display voltage range do not change detection thresholds.
- Voltage conversion is (ADC - 32768) * 0.195 uV. With filtering enabled these
  are filtered voltages, not the original raw ADC values.
- Each event retains the crossing sample index, absolute source sample time,
  recorded stream and chip channel, signed post-crossing peak and peak sample,
  snippet peak-to-peak voltage, sampling rate, pre-window length and every
  full-rate snippet sample. Channel and chip indices in CSV are zero-based.
- At 30 kHz, a 1 ms pre-window and 2 ms post-window contain 91 samples including
  the crossing. The peak is searched from crossing through the post-window; the
  peak-to-peak value covers the complete snippet. No waveform alignment jitter
  is introduced by display binning. Filter group delay is not corrected.
- Discontinuities and flagged stimulation-artifact blocks reset filter/detector
  history. They are not bridged with fabricated snippets. Filter warm-up is 20 ms
  after a reset. Pending snippets at a gap or session end are discarded and counted.
- The detector works on actual input channel count (16/64/120 in software); selecting
  a larger MEA picture does not create hardware channels.

## Recording And Limits

Continuous CSV is written by the backend, independently of UI polling. It includes
waveforms as semicolon-separated uV values in the last column. Buffered output is
flushed about once a second and at orderly session end. Crashes, power loss or
storage failure can leave an incomplete file. The UI reports file errors; a file
error does not disable acquisition or silently claim successful recording.

The backend UI event cache is 4096 events; polling returns at most 512 at a time.
The frontend retains at most 20000 events and overlays up to eight recent snippets
per channel. The numerical table shows the latest 200 events. The export button
exports **only the current frontend cache**, not an entire session. Cache eviction,
polling loss and incomplete snippets are reported. Counts are session totals even
after old events leave the UI cache. Raster rates are window counts divided by the
selected window duration, and can be incomplete when cache history was lost.
Playback seeks/restarts and pipeline changes begin a new UI analysis segment,
clearing counts and snippets so time ranges/filter settings are not mixed. A
continuous CSV stays open across these segments; event IDs remain unique, and a
replayed source sample can appear again with a new event ID.

Browser preview is synthetic and labeled as such. It cannot write a continuous
recording. Unit tests and UI simulation do not establish detection sensitivity,
specificity or end-to-end hardware throughput on biological recordings.
# Continuous Waveform Markers

## Noise-Referenced Detection

New sessions default to per-channel noise-multiple detection. Legacy serialized
settings without `thresholdMode` remain fixed-microvolt detection. Users can
select either mode, positive/negative polarity, a multiplier (default 5), and a
calibration interval (default 1 second). These are software defaults, not validated
parameters for a biological preparation.

After the detector's filter warm-up, each channel samples its actual detection
signal across the calibration interval, retaining at most 4096 values. Its robust
baseline is the median, and sigma is median(abs(x - median(x))) / 0.67448975.
The threshold is baseline +/- multiplier * sigma. This follows the robust
noise-estimation approach used for extracellular spike detection, e.g.
https://pmc.ncbi.nlm.nih.gov/articles/PMC3213406/ .

Thresholds are frozen after calibration for that continuous segment, not silently
retuned after each spike. No automatic candidates are emitted during calibration.
Flat, insufficient and saturated calibration data yield no usable threshold.
Gaps, stimulation-artifact blocks, seeks and processing-chain resets require
recalibration. Fixed-threshold behavior and refractory/snippet handling remain.

Both event CSV paths include the actual threshold and estimated noise of each
candidate. Raw-file configuration metadata includes the detection settings and
available per-channel thresholds at recording start. Replay reapplies settings
and recalibrates from replayed samples; start-threshold metadata is a reference,
not an instruction to reuse output permissions or frozen noise from another signal.

The activity map now uses detector events only: zero events are dark gray, pulse
mode fades recent events over 100/200/500 ms, and rate mode counts events over
1/5/10/30 s against a configurable fixed Hz color scale. Pulse fading continues
when the source stops; rate mode retains the last source-time window for inspection.
It no longer converts analog noise amplitude into apparent firing activity.

The spike view supports an all-channel raster, individual/all-channel snippets
with threshold dashes and red peak dots, and a numerical event table. Threshold
lines are not projected onto a different signal: when the detector uses its own
band-pass, its threshold is drawn on detection snippets, not the raw waveform.

The waveform view overlays detected candidate spikes as red dots in overview and
single-channel modes. The marker switch controls display only. Use the detection
settings button to open the existing all-channel detector settings; apply them
before starting acquisition or playback. No hardware stimulation is involved.

Markers use `peakSampleIndex`, matched to retained waveform frames by detector
epoch and sample rate. They do not use UI polling time or threshold-crossing time.
The detector reports `displayPeakUv` from its unmodified input at that exact peak
sample, separately from `peakUv` after its optional independent band-pass filter.
Cross-block candidate windows retain the corresponding display input values.

Markers outside the displayed voltage/time range, hidden overview channels,
missing waveform intervals and stimulation-artifact blocks are not drawn. New
acquisition, replay seeking and processing-chain rebuilds isolate old epochs.
Pausing the waveform freezes its marker metadata too; moving the pointer over a
dot shows channel, peak time, detection amplitude and current-waveform amplitude.
Waveform-mode event polling runs at about 50 ms so short time windows can display
new events; non-waveform views retain the lower polling cadence.

The preview uses explicitly synthetic events generated on the same sample clock
and injected waveform points, not an independent wall-clock event generator.
Raw recordings, spike CSV columns, thresholds and refractory behavior are unchanged.
