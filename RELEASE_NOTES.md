# NeuroX Studio 2026.11.05-hardware1

Windows x64 portable research release. Download the Windows ZIP and extract it
fully; run `NeuroX Studio.exe`. This is not a Setup installer. The frontend is
embedded and does not require a localhost development server.

This hardware bundle adds the matching lab controller's ConfigRHSController.bit,
the original FrontPanel 5.2.10 x64 USB driver directory and an explicit,
administrator-run Install-USB-Driver.cmd helper. No acquisition/stimulation code
has changed since 2026.11.05. The previous minimal release remains unchanged.

## Included

- 16/64-channel native acquisition with ordered processing and raw recording.
- Two-column 64-channel overview, individual channel detail and independent or
  apply-to-all absolute microvolt ranges with session recovery.
- MEA probe maps, impedance tools, spike detection/markers, frequency analysis,
  recording replay and full-recording analysis export.
- Standalone source tools for RHSRAW-to-Intan-RHS conversion.
- Explicitly acknowledged, finite 16/64-channel hardware stimulation controls.

## Before running

- Install Microsoft WebView2 Evergreen Runtime and the latest supported Visual
  C++ v14 Redistributable x64 if not already installed. Links are in README.
- Keep the bundled `okFrontPanel.dll` (5.2.10 x64) beside the EXE.
- For the matching lab hardware, the driver and bitfile are now included.
  If no working USB driver is installed, right-click Install-USB-Driver.cmd and
  run as administrator, then confirm installation. Keep the bitfile beside the EXE.
  See HARDWARE_SETUP.md; do not use this bitfile on a different controller/FPGA.
- Microsoft runtimes are still separate downloads, not included installers.
- Rust, Node.js, VS Code and the full Intan application are not runtime requirements.

## Limitations

120-electrode support is display/preview only, not validated 120-channel hardware
acquisition or stimulation. Software tests do not validate electrode current,
phase duration, compliance, emergency-stop performance on an actual board or
biological safety. Follow the bench protocol before experimental use. Stimulation
is not automatically armed by installing or opening this release.

The EXE is unsigned. Verify the SHA-256 checksums and download origin; do not
disable Windows security protections. Corresponding source, licenses and a file
manifest are provided. Existing recordings are not modified by installation.
