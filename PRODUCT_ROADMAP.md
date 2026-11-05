# NeuroX Studio Product Roadmap

## Product Definition

NeuroX Studio is a desktop application for multichannel electrophysiology acquisition,
real-time processing, recording, analysis and safely controlled stimulation. The UI,
runtime, experiment workflow and data formats are designed and implemented in this
project. Hardware SDKs and FPGA images remain isolated behind explicit adapters.

Status values:

- `DONE`: implemented and verified at the stated level.
- `PARTIAL`: a usable subset exists, but the complete workflow is not verified.
- `PLANNED`: not implemented yet.
- `HARDWARE`: implementation or acceptance requires connected hardware and measurement.

## V1 Core Experiment

| Capability | Status | V1 acceptance |
|---|---|---|
| Device discovery and FPGA configuration | DONE | Enumerate, open, configure, scan and release the controller with structured errors. |
| Single-chip acquisition | DONE | Acquire 16 RHS2116 channels at 30 kHz without timestamp gaps or unbounded memory. |
| Multichip acquisition | HARDWARE | The software path accepts up to 120 channels; the native hardware adapter still requires synchronized multistream acquisition and acceptance. |
| Channel and MEA profiles | DONE | Select, edit and persist 16/64/120 layouts, names, mapping, enable state, groups and physical coordinates. |
| Live waveform display | DONE | Bounded envelope display with gain, time window, pause and 16-channel pagination for 16/64/120-channel sources. |
| Ordered processing pipeline | DONE | Add, remove, reorder and configure typed processing nodes without stopping raw recording. |
| Raw recording | DONE | Write versioned RHSRAW blocks with CRC, metadata, continuity and completion markers. |
| Desktop playback | DONE | Open RHSRAW recordings, seek, change speed, play and pause through the same display and analysis path. |
| Triggered recording | PARTIAL | Pre/post buffers and software/digital/analog trigger detection are implemented; physical trigger input still needs hardware acceptance. |
| Stimulation configuration | DONE | Configure selected MEA channels, validated presets, finite pulse trains, waveform and charge recovery. |
| Arm, Trigger, Cancel and Disarm | HARDWARE | The guarded sample-clock scheduler and desktop workflow are complete in interlocked mode; physical output still needs test-load validation. |
| Emergency Stop | HARDWARE | Software cancellation and state transitions are complete; physical stop latency and failure behavior require measurement. |
| Impedance measurement | HARDWARE | The selection, validation, status and result UI are ready; actual RHS measurement and result export require the hardware path. |
| Experiment session | DONE | Versioned presets and recovery save session/operator/subject notes, layout, pipeline, stimulus, acquisition, recording, analysis and tool settings. |

## V2 Analysis

| Capability | Status | Acceptance |
|---|---|---|
| Notch, band-pass and artifact blanking | DONE | Stateful block processing matches deterministic offline tests. |
| Threshold event detection | DONE | Emit bounded, sample-indexed events with refractory control. |
| Spike scope and raster | DONE | Render bounded live event rasters, rates and captured waveform snippets from sample-indexed events. |
| Spectrum and spectrogram | DONE | Compute and render bounded FFT/PSD and time-frequency views without blocking acquisition. |
| ISI and PSTH | DONE | Compute reproducible histograms from recorded or live sample-indexed events. |
| MEA signal-activity heatmap | DONE | Render normalized live envelope activity and per-channel measurements on the active MEA geometry. |
| Spike-rate heatmap | DONE | Render event rate on the active MEA geometry using the selected analysis window. |
| Probe/MEA map editor | DONE | Edit coordinates, labels, groups, mapping and enable state, with profile persistence. |
| Data export | DONE | Export workspace metadata, channel measurements, events and analysis summaries as JSON, CSV, HTML and Excel XML. |

## V3 Integration And Operations

| Capability | Status | Acceptance |
|---|---|---|
| Digital and analog I/O | HARDWARE | Configuration, display, edge accounting, trigger integration and output interlocks are implemented; physical I/O requires hardware acceptance. |
| Audio monitor and analog output | PARTIAL | Web Audio routing includes channel selection, DC removal, resampling, gain, mute and limiting; physical signal monitoring remains unverified. |
| Remote control and streaming | PARTIAL | A token-authenticated, versioned localhost TCP API exposes bounded status and event queries; waveform streaming and the wider command set remain. |
| Reusable experiment protocols | PARTIAL | Versioned pipeline, stimulation and complete workspace presets exist; timed multistep protocol scheduling remains. |
| Diagnostics and recovery | PARTIAL | Structured events, diagnostics export and non-activating workspace recovery exist; unplug, disk-full and crash fault injection remain. |
| Performance qualification | PARTIAL | Bounded queues, deterministic tests and responsive desktop checks pass; latency, disk-capacity and multihour soak reports remain. |
| Windows distribution | PARTIAL | Local Release installer exists; add dependency checks, versioning, uninstall and signed packages. |
| Audit and reproducibility | PARTIAL | Record software, DLL, FPGA, configuration and protocol hashes with each experiment. |

## Delivery Order

1. Validate physical stimulation and Emergency Stop timing on a test load before biological use.
2. Extend the native adapter from one 16-channel stream to synchronized multistream acquisition.
3. Connect and accept physical digital/analog triggers, I/O, impedance and audio monitoring.
4. Add timed multistep protocol scheduling and expand the authenticated local API.
5. Run unplug, disk-full, crash, latency and multihour soak qualification.
6. Finish uninstall, package signing and deployment documentation.

No feature is marked complete solely because a control is visible. Hardware-facing
features require measured hardware evidence; analysis features require deterministic
tests against saved data; operational features require explicit failure-path testing.
