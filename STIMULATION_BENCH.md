# 16/64-channel stimulation research build

This build provides real hardware triggering, not just the interlocked preview.
It provides bench validation and configurable research output, not a qualified
biological stimulation device.
No physical output measurement is implied by a passing software test.

## Supported addresses

16-channel mode uses one detected RHS2116. The 64-channel MEA mode selects the
four RHS2116 chips at command streams 2, 3, 4 and 5. Additional chips on A/D do
not prevent operation; their data streams and stimulation triggers remain disabled
for this profile. Missing B/C streams are never replaced by another port:

| Port-local channel | Command stream | Chip channel |
| --- | --- | --- |
| B-000 through B-015 | 2 | 0 through 15 |
| B-016 through B-031 | 3 | 0 through 15 |
| C-000 through C-015 | 4 | 0 through 15 |
| C-016 through C-031 | 5 | 0 through 15 |

Electrode-site positions come from the separate probe map. A successful scan
does not identify which electrode is connected to a dummy load.
120/128-channel acquisition is not supported by this build. An eight-chip board
can be used in the B/C 64-channel profile; this is not an all-128-channel mode.

## Desktop workflow

1. Close other applications that own the FrontPanel device.
2. Select RHS2116 or MEA 64, scan, and inspect the detected chip list.
3. Fill in the operator and configure selected channels and stimulation parameters
   before acquisition. Session fields are locked while acquisition is running.
4. Start acquisition. The preset is programmed but output remains disabled.
5. For a verified dummy-load bench, choose the measured validation mode and explicitly
   confirm sample disconnection, dummy-load connection and measurement readiness.
   Enable real bench output, then Arm. Arm itself sends no pulse.
6. Trigger sends a finite sequence to the selected physical addresses. Inspect
   every selected output on the measurement equipment and inspect the audit.
7. Cancel, Disarm or Emergency Stop disables output and latches stimulation off.
   Restart acquisition to arm again or to change preloaded channels/parameters.

### No-biological-sample functional check

The separate desktop validation mode `No-bio single-channel functional check`
does not require dummy-load or instrument confirmations. It checks command
delivery and FPGA feedback, not measured output current. Select this mode only
after disconnecting all biological samples; do not mark instrument confirmations
unless they are true.

Select only B-000 in the MEA 64 profile (command stream 2, chip channel 0).
Use the single biphasic preset: 10 nA, 200 us per phase, 100 us gap, 10 Hz,
one pulse. Start acquisition to preload it, select functional validation, confirm
sample disconnection and real output, then Arm and click Trigger once. Arm alone
sends no pulse. This mode permits only one non-repeating request per Arm, with at
most 1,000 ms delay. Parameter changes require stopping and restarting acquisition.

For bounded retrigger testing only, explicitly select the separate no-bio repeated
functional mode. It keeps the same one-channel, one-pulse preset and permits at
most three individually clicked Trigger submissions within the same 60-second
Arm lifetime, at least one second apart. There is no automatic pulse train in
this mode. The submission budget is consumed when a command is submitted, not
only when completion is reported; rejection does not grant extra attempts.
The original single-shot mode still permits only one submission. Neither mode
is a biological-sample stimulation mode, and neither measures delivered current.

Successful functional feedback requires Queued, Triggered, Observed and Completed.
Triggered alone is not success. Faults stop output; do not automatically retry.
The audit explicitly distinguishes functional validation from a measured bench
test. Even Observed and Completed do not establish actual current or waveform.

### Configurable real output (2026-10-23)

The separate `Configurable real output / research` mode accepts operator-selected
16/64-channel masks and supported parameters. It does not require or invent
dummy-load, instrument or sample-disconnection confirmations. Instead, the
operator explicitly confirms routing/return connections, reviewed parameters
and acknowledgement that physical output and biological suitability are
unverified. This selection is not laboratory approval or device qualification.

Stop acquisition, open the channel/parameter editor, select channels, save the
preset and enter an operator name. Start acquisition to preload the preset, then
select configurable real output in the stimulation panel and explicitly check
real output and all three acknowledgements. Arm shows the quantized hardware
parameters and charge per phase; Arm itself does not trigger. Trigger submits
the finite sequence. Parameter or channel changes require stopping acquisition
and reloading the preset; changing output mode clears confirmations.

This mode retains the existing 10..10,000 nA exactly representable current range,
1..20 pulses per sequence and symmetric negative-first biphasic waveform, with
an optional gap. Requested phase/gap values are capped at 10,000 us and quantized
to the 30 kHz clock. Delayed biphasic requires a positive gap. Triphasic and
active charge recovery are not implemented. These are implementation bounds,
not cell-culture safety limits.

Each Arm expires after 60 seconds and permits at most 16 submitted finite
commands cumulatively. The remaining lifetime must include sequence duration
and a 100 ms margin. Further submissions wait for the previous scheduled span
plus 100 ms; scheduled repeats also require at least 100 ms beyond occupied
sequence duration. Cancel, Disarm, Emergency Stop and faults latch output off;
restart acquisition before another hardware Arm. The audit stores requested and
quantized settings, the real confirmations, `currentMeasured=false` and
`qualifiedForBiologicalUse=false`. Neither startup nor mode selection triggers.
The browser preview rejects real-output confirmation payloads.

### Trigger-source correction (2026-10-22)

Intan manual trigger indices 0 through 7 map to sequencer trigger sources 24
through 31, not 0 through 7 (which are digital inputs). The shared native
sequencer now uses this offset. A fake-controller regression models manual
edges and verifies that digital source 0 does not fire from manual trigger 0,
whereas source 24 does. This offline regression does not replace a hardware test.

The earlier one-shot B-000 functional attempt queued and triggered, but failed
because no FPGA stim-on marker was received. Its audit is preserved as
`work/stim64-functional-20261022-B000-once.jsonl`.

Subsequent user-operated RHS Studio checks on the test unit (serial redacted) recorded three complete
Queued / Triggered / Observed / Completed chains for B-000 only. The selected
stream mask was [0,0,1,0,0,0,0,0], and observed scheduling error was 674 through
709 samples (22.467 through 23.633 ms). These are functional results, not current
measurements or all-64-channel output validation. Later submissions disconnected
the acquisition worker without completed feedback, so stability is not qualified.

### Worker-disconnect diagnostics

The old desktop read only one worker status at disconnect. A stale Started
status could hide the later Fault, producing only `acquisition worker disconnected`.
The client now drains startup statuses, preserves the terminal device error and
panic detail, drains remaining stimulation events and publishes final gap/overflow
counters before reporting the fault. These changes repair diagnostics, not a
proven underlying USB or hardware failure. A fresh desktop reproduction is needed
to identify that failure. Later user-operated diagnostic-build single-shot logs
also contain successful completion; this does not establish a root cause or
repeat/stop stability. No new pulse was sent by the software regression tests.

Arm is disabled and rejected while already armed, so it cannot reset the current
Arm lifetime, completion count or audit. Stimulation controls are also disabled
while an Arm/Trigger/Cancel/Disarm request is in flight. A completed functional
check requires explicit Disarm and acquisition restart before a new hardware Arm.
Emergency Stop remains independently available. Stop testing on any new Fault;
do not repeatedly restart and trigger to work around it.

The production acquisition loop is now exercised with an injected software I/O
transport for repeat, emergency-stop and failure regressions. Three sequential
commands, queued-command cancellation, active-plus-future cancellation, trigger
failure and read failure pass without hardware output. Terminal failures now
latch the session and report Fault for affected commands rather than leaving
them pending. These tests do not measure real hardware emergency-stop latency.

## Before biological samples

An empty MEA or a flat recording is not a known electrical dummy load and does
not verify stimulation. The measured-bench gate must not be acknowledged unless
the selected load and measurement setup are actually present. A measurement
record needs the physical channel, measured load resistance, instrument/probe
setup, both phase polarities and amplitudes, phase widths, gap and repeated-pulse
behavior. Compare current inferred from voltage across a known load with the
requested current, accounting for instrument resolution, probe loading and
calibration. Do not derive delivered current from the ordinary AC neural trace
or from FPGA stim-on bits. Do not improvise connections to exposed PCB points.

For a cell-culture MEA experiment, the responsible experimenter must also verify
electrode material, exposed area, impedance, return path and the laboratory's
approved protocol and limits. Nominal symmetric biphasic programming alone does
not establish physical charge balance or replace active charge recovery.
The bench and no-bio functional modes still require disconnected biological
samples. The separate configurable research mode does not assert disconnection;
it records explicit unverified-output acknowledgement instead. Passing software
tests or selecting that mode does not establish readiness for cell experiments.

Arm expires after 60 seconds. No startup, scan, preset selection or browser
preview automatically triggers real stimulation. The portable executable embeds
the frontend; no Node server or localhost web page is required.
Keep the executable, bitfile and supplied DLLs together. The target PC needs the
FrontPanel USB driver, Microsoft Edge WebView2 Runtime and the x64 Visual C++
runtime; this portable archive does not install system drivers or runtimes.

## Waveform and limits

- Shared, symmetric, negative-first biphasic waveform, optionally with a gap.
  Independent per-channel waveforms and triphasic stimulation are not supported.
- Balanced biphasic does not mean active charge recovery. Active recovery is
  not programmed in this build.
- Current must be exactly representable by a supported 10/20/50/100 nA DAC step
  with 1 through 255 magnitude steps; the request range is at most 10,000 nA.
- Timing is quantized to the 30 kHz sample clock. Each phase is 1 through 1,000
  samples, gap 0 through 1,000, period 10 through 30,000. The period must exceed
  both phases and the gap. A nonzero requested gap is at least one sample.
- Finite pulse count is 1 through 20; period times pulse count plus 256 must
  fit in 65,534 samples. Repeated requests are limited to 16 commands, less
  than 60 seconds, with at least 100 ms between occupied sequences.
- Host scheduling and USB stop latency are not hard real-time guarantees.
  Actual current, timing, channel isolation and stop latency require measurement.

## Feedback and audit

The acquisition worker retains 64-bit stim-on and compliance masks. Completion
requires FPGA stim-on feedback from every selected channel, not just one chip.
Selected-channel compliance faults, missing markers, timestamp discontinuities
while stimulation is pending, queue overflow and audit failure stop output.
FPGA feedback is not proof of current delivery into the load.

If RHS2116 reports a selected-channel compliance warning, output is latched off
and acquisition stops. A static waveform then represents the last frame, not a
frozen stimulation command or a guarantee of current delivery. The view displays
the stop reason, and fault diagnostics retain completed command counts and audit
path. Review the physical channel, electrode/return/load and experiment setup
before another session; do not bypass compliance protection or repeatedly retry.

Audit JSONL files are stored in the application's data directory under
`stimulation-audit`. The active path is displayed in the stimulation panel.
Each Arm records operator confirmations, device serial, streams, selected masks,
requested and quantized parameters; subsequent records contain scheduling intent,
hardware event sample indices and explicit cancellation/stop/expiry actions.
Records are flushed after writes, but are not guaranteed against sudden power loss.
