# RHS Rust Control

Rust workspace for the RHS2116 control path:

```text
Rust CLI
-> rhs-device
-> C ABI bridge
-> Intan RHXController C++ API
-> FrontPanel
-> FPGA bitfile
-> RHS2116
```

The current workspace also contains the first acquisition pipeline foundations:

```text
rhs-core       SampleBlock, events, structured device errors
rhs-device     persistent acquisition session and single hardware worker
rhs-pipeline   Source/Processor/Sink runner, cancellation and bounded queue
rhs-nodes      filters, envelope, blanking, threshold events and viewer frames
rhs-storage    versioned raw recording, CRC, metadata and file replay
rhs-stim       armed safety policy, preset scheduler, cancellation and audit
rhs-runtime    sample-clock stimulation adapter for the acquisition worker
rhs-cli        scan, acquisition, recording and replay diagnostics
rhs-desktop    Tauri 2 desktop workbench and bounded waveform IPC
```

Current verified hardware:

```text
FrontPanel device: one locally connected unit (serial redacted)
Detected chip: RHS2116 on port D, command_stream=6
```

## Local setup

The FrontPanel SDK and FPGA bitfile are workstation dependencies and are not committed.
Create `scripts\local-env.bat` from `scripts\local-env.example.bat`, then configure:

```text
FRONTPANEL_SDK=C:\path\to\FrontPanelUSB
RHS_BITFILE=C:\path\to\ConfigRHSController.bit
```

Without `RHS_BITFILE`, commands look for `ConfigRHSController.bit` in the current working
directory. The device serial is optional; leaving it empty selects the first FrontPanel
device detected by the SDK.

## Configurable desktop pipeline

RHS Studio uses a versioned, ordered built-in-node configuration:

```text
RHS Source -> enabled nodes in UI order -> bounded waveform view
           \-> raw RHSRAW recorder (before processing)
```

The desktop editor can add, remove, enable, disable, reorder and configure artifact
blanking, notch, band-pass and threshold nodes. Each saved local preset contains the
pipeline version, stable node IDs, order and parameters. The Rust backend validates the
full configuration against the 30 kHz/16-channel stream before replacing the active
processing chain; rejected edits leave the previous chain running.

The node registry is a Rust/Tauri API, so adding another built-in processor consists of a
new typed config variant, registry definition, validation and processor factory branch.
Dynamic DLL nodes are intentionally deferred. Raw recording and stimulation safety are
separate services: pipeline nodes cannot alter the raw branch or bypass Arm/Trigger policy.

## Run

Launch the native RHS Studio desktop app from this directory:

```powershell
.\scripts\dev-desktop.bat
```

The app starts idle. Scan the device before acquisition; acquisition always starts with
stimulation disabled. Arm and Trigger remain locked until the skipped bench-output
validation is completed. Emergency Stop is always available.

For frontend-only design work, run the browser preview:

```powershell
cd apps\rhs-desktop
npm run dev
```

Open `http://127.0.0.1:1420`. This preview uses generated mock data and never opens the
FrontPanel device. The native Tauri backend keeps full-rate samples in Rust and sends only
a bounded min/max waveform envelope to the WebView.

From this directory:

```powershell
.\scripts\dev-cmd.bat scan
```

Read-only continuous acquisition diagnostic:

```powershell
.\scripts\dev-cmd.bat acquire --seconds 5
```

Record real RHS2116 data through the pipeline (the output path must not already exist):

```powershell
.\scripts\dev-cmd.bat record --seconds 5 --output .\work\trial-001.rhsraw
```

Replay and verify every block, CRC, footer, timestamp continuity and bitfile fingerprint:

```powershell
.\scripts\dev-cmd.bat replay-check --input .\work\trial-001.rhsraw
```

Run the real-time notch, band-pass, blanking, threshold and bounded Viewer path:

```powershell
.\scripts\dev-cmd.bat viewer-check --seconds 5
```

`viewer-check` intentionally leaves the four-frame display queue unread during acquisition. Old display frames are dropped while hardware blocks remain contiguous, which exercises the slow-UI policy without sending stimulation.

Synthetic source and bounded queue diagnostic:

```powershell
.\scripts\dev-cmd.bat synthetic-check --blocks 1000
```

Run Rust workspace commands in the required Visual C++ environment:

```powershell
.\scripts\dev-cargo.bat test --workspace
.\scripts\dev-cargo.bat clippy --workspace --all-targets -- -D warnings
```

Run the stimulation safety state machine without hardware output:

```powershell
.\scripts\dev-cmd.bat stim-service-check
```

Open and close the real stimulation session while keeping stimulation disabled:

```powershell
.\scripts\dev-cmd.bat stim-session-check
```

Preload a finite stimulation preset in the unified acquisition session and verify that
continuous acquisition remains clean without scheduling a trigger:

```powershell
.\scripts\dev-cmd.bat sync-session-check --seconds 2
```

The acquisition worker now owns the synchronized stimulation command queue and returns
FPGA `stim_on`, polarity, compliance and DC-amplifier metadata with each sample batch.
`rhs-runtime` adapts that queue to the `rhs-stim` Arm/preset/audit service. The historical
synchronous `run_bounded_stim` API remains excluded from default builds.

Real output is restricted to fixed, finite bench presets. Do not run these commands until the dummy load wiring and oscilloscope/current measurement are ready and approved. Validate the single pulse before the five-pulse train:

```powershell
.\scripts\dev-cmd.bat stim --preset bench-single-v1 --operator NAME --confirm-dummy-load --confirm-measurement-ready
.\scripts\dev-cmd.bat stim --preset bench-train-v1 --operator NAME --confirm-dummy-load --confirm-measurement-ready
.\scripts\dev-cmd.bat stim --preset bench-train-v1 --operator NAME --confirm-dummy-load --confirm-measurement-ready --emergency-stop-after-ms 150
```

Arbitrary stimulation parameters and biological-sample confirmation are intentionally not accepted by this CLI. See `ITERATION4_BENCH_PROTOCOL.md` before using either real-output command.

The `acquire` and `record` commands explicitly disable stimulation. Inputs that are not connected to electrodes may contain floating, saturated, or full-range raw values; this does not validate biological signal quality.

See `PLAN.md` for the development roadmap and `VALIDATION.md` for verified results.

## Third-party code

`vendor/intan-rhythmstim-api-230822` contains the Intan Technologies RHXController API
sources used by the native bridge. Those files are distributed under GPL-3.0; the
corresponding license text is included beside them. The proprietary FrontPanel SDK,
drivers, DLLs and FPGA bitfiles are intentionally not included.
