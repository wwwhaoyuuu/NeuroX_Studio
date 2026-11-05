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
GPL. FrontPanel SDK headers/import libraries and USB driver installers are not
included in the source archive. See the official distribution guidance:
https://docs.opalkelly.com/fpsdk/licensing/ .

The interface uses Lucide icons (ISC license), bundled from the pinned npm
dependency. Its license is included at `licenses/Lucide-LICENSE` in releases.
Other Rust dependencies retain their own licenses; exact versions are recorded
in `Cargo.lock`. Microsoft WebView2 and Visual C++ runtimes are separate Microsoft
components, installed from Microsoft, not redistributed in this package.

Board-specific FPGA bitfiles, experiment recordings, personal channel settings,
credentials and local workstation paths are not included in public releases.
