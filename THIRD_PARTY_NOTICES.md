# Third-Party Notices

NeuroX Studio uses the Intan RHX/RhythmStim C++ controller API. Original copyright
and license notices remain in `vendor/intan-rhythmstim-api-230822/`. These files
are licensed under GNU GPL version 3 or, at your option, any later version.
The corresponding NeuroX source is provided alongside each binary release and
in this repository; the combined application is distributed under those GPL
terms. See `COPYING`. This software is provided without warranty.

Local adaptations include stream selection and compact packet indexing, finite
stimulation sequencing, hardware-worker ownership/cancellation and impedance
measurement. Source and build scripts for these changes are included. NeuroX
Studio is a separate application, not an official Intan or Opal Kelly product.

The Windows binary depends on Opal Kelly FrontPanel. The distributed x64
`okFrontPanel.dll` is version 5.2.10 and retains its separate proprietary license
in `licenses/FrontPanel-LICENSE.txt` inside the portable package. It is distributed
for use with Opal Kelly devices or devices incorporating its firmware, under the
FrontPanel License Agreement. This does not relicense the FrontPanel SDK under
GPL. Hardware bundles also contain the unmodified amd64 USB/UART driver files
from FrontPanelUSB 5.2.10 under that same FrontPanel license, including the
original signed catalogs and coinstallers. The optional installation helper
installs only the USB INF through Windows PnPUtil. FrontPanel SDK headers/import
libraries, drivers and runtime DLLs are not included in the source archive.
See the official distribution guidance:
https://docs.opalkelly.com/fpsdk/licensing/ .

The interface uses Lucide icons (ISC license), bundled from the pinned npm
dependency. Its license is included at `licenses/Lucide-LICENSE` in releases.
Other Rust dependencies retain their own licenses; exact versions are recorded
in `Cargo.lock`. Microsoft WebView2 and Visual C++ runtimes are separate Microsoft
components, installed from Microsoft, not redistributed in this package.

The 2026.11.05-hardware1 binary bundle includes the existing lab controller's
board-supplied ConfigRHSController.bit, at the hardware owner's request, for the
matching hardware. Original vendor rights remain; inclusion grants no new
firmware license and does not place that binary under GPL. Its exact SHA-256,
provenance and scope are recorded in hardware-payload.json. No FPGA source or
general-purpose compatibility is claimed. The bitfile is not in the source ZIP.
Experiment recordings, personal settings, credentials and local workstation
paths are excluded from both release ZIPs.
