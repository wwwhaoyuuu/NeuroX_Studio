# NeuroX Analysis Export

For actual Intan `.rhs` conversion, use the standalone `rhsraw_to_rhs.py` script.
See [RHS_CONVERSION.md](RHS_CONVERSION.md) for commands, verification and the
unavailable stimulation fields. Renaming a suffix alone is not conversion.

This directory contains full-rate raw data, not the decimated display buffer.
The original recording is unchanged. No software filtering is applied during
export. Hardware analog filtering is already part of the acquired signal.

- `parameters.json`: acquisition rate, physical channel order, probe/workspace
  configuration at recording start, units and export shape.
- `samples.u16le`: little-endian uint16 ADC words, sample-major, rows x channels.
- `sample_index.u64le`: original sample index for each row.
- `ttl_in.u16le`: digital input word for each row. Consult `ttl_present` below;
  missing TTL is represented by zero, not an observed digital low.
- `blocks.csv`: block sequence, zero-based output row, count, original sample
  index, flags and TTL availability. Flags 1/2/4 denote discontinuity,
  stimulation artifact and device warning.
- `samples.csv` (optional): sample index, relative time in seconds, TTL, flags,
  and one column per physical channel in microvolts.
- `read_neurox.py` and `read_neurox.m`: Python/NumPy and MATLAB readers.

An `INCOMPLETE.txt` marker means export failed or is still running. Do not analyze
it. Select a new destination and export again. Existing directories are never
overwritten. A complete export has no incomplete marker.

ADC conversion: `microvolts = (adc - 32768) * 0.195`. Convert to floating point
BEFORE subtraction. Source metadata/probe positions do not change matrix order;
use `channels` to resolve each column's stream and chip channel. Unknown/legacy
probe metadata is not invented. `configuration` may be null for older recordings.

Time in seconds is `(sample_index - firstSampleIndex) / sampleRateHz`. Do not
replace it with row number / sample rate when there are missing intervals.
Split analysis at sample-index gaps and flagged block boundaries. Do not
interpolate missing samples or filter across gaps. Mark stimulation artifacts
before spike detection. Threshold events are candidates, not sorted neuron IDs.

## Python

Python needs NumPy; plotting additionally needs Matplotlib. These scripts do not
install packages or contact any external service.

```python
from read_neurox import read_neurox
import numpy as np
import matplotlib.pyplot as plt

d = read_neurox(r"C:\data\trial-analysis")
p = d["parameters"]
channel = d["channels"].index("B-09")
# First block only: a continuous interval, without crossing block flags/gaps.
b = d["blocks"][0]
rows = slice(b["row_start"], b["row_start"] + b["sample_count"])
uv = (d["adc"][rows, channel].astype(np.float64) - p["adcZero"]) * p["microvoltsPerAdc"]
t = (d["sample_index"][rows].astype(np.float64) - p["firstSampleIndex"]) / p["sampleRateHz"]
plt.plot(t, uv)
plt.xlabel("Time / s")
plt.ylabel("B-09 / microvolts")
plt.show()
```

The Python reader memory-maps data; select channels/time intervals before
converting to floating point. MATLAB's reader loads the raw arrays in memory.
For very large files, use `memmapfile` or block-wise `fread` in MATLAB instead.

## MATLAB

```matlab
d = read_neurox('C:\data\trial-analysis');
p = d.parameters;
ch = find(strcmp(d.channels, 'B-09'));
b = d.blocks(1, :);
rows = (b.row_start + 1):(b.row_start + b.sample_count);
uv = (double(d.adc(rows, ch)) - p.adcZero) * p.microvoltsPerAdc;
t = (double(d.sample_index(rows)) - p.firstSampleIndex) / p.sampleRateHz;
plot(t, uv); xlabel('Time / s'); ylabel('B-09 / microvolts');
% Optional CSV is also readable with readtable('samples.csv').
```

CSV is convenient but much larger/slower than binary for 64-channel recordings.
Keep the original `.rhsraw` as the lossless archive. Neither changing a suffix
nor these exports creates an Intan `.rhs` file.
