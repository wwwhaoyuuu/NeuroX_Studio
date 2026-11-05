function finiteNumber(value, fallback = 0) {
  const number = Number(value);
  return Number.isFinite(number) ? number : fallback;
}

function percentile(sorted, fraction) {
  if (!sorted.length) return 0;
  const index = Math.min(sorted.length - 1, Math.max(0, Math.round((sorted.length - 1) * fraction)));
  return sorted[index];
}

export function frameChannelActivity(frame) {
  const channelCount = Math.max(0, Math.floor(finiteNumber(frame?.channelCount)));
  const pointCount = Math.max(0, Math.floor(finiteNumber(frame?.pointCount)));
  const requiredLength = channelCount * pointCount;
  if (!channelCount || !pointCount) return [];
  if (!Array.isArray(frame.minimums) || !Array.isArray(frame.maximums)) return [];
  if (frame.minimums.length < requiredLength || frame.maximums.length < requiredLength) return [];

  const activity = Array.from({ length: channelCount }, () => ({
    score: 0,
    averagePeakToPeak: 0,
    peakDeviation: 0
  }));
  for (let channel = 0; channel < channelCount; channel += 1) {
    let rangeSum = 0;
    let peakDeviation = 0;
    for (let point = 0; point < pointCount; point += 1) {
      const index = point * channelCount + channel;
      const minimum = finiteNumber(frame.minimums[index], 32768);
      const maximum = finiteNumber(frame.maximums[index], 32768);
      rangeSum += Math.max(0, maximum - minimum);
      peakDeviation = Math.max(peakDeviation, Math.abs(minimum - 32768), Math.abs(maximum - 32768));
    }
    const averagePeakToPeak = rangeSum / pointCount;
    activity[channel] = {
      score: averagePeakToPeak * 0.65 + peakDeviation * 0.35,
      averagePeakToPeak,
      peakDeviation
    };
  }
  return activity;
}

export function normalizeActivity(activity) {
  const scores = activity.map((entry) => Math.max(0, finiteNumber(entry?.score)));
  const positive = scores.filter((value) => value > 0).sort((left, right) => left - right);
  if (!positive.length) return scores.map(() => 0);
  const lower = percentile(positive, 0.1);
  const upper = Math.max(lower + 1, percentile(positive, 0.9));
  return scores.map((value) => Math.min(1, Math.max(0, (value - lower) / (upper - lower))));
}

export function spikeActivitySummary(events, endMs, windowMs, channelCount, wallAgeMs = 0, flashMs = 200) {
  const counts = Array(channelCount).fill(0), flashes = Array(channelCount).fill(0);
  const start = Math.max(0, endMs - windowMs);
  for (const event of events || []) {
    const channel = event.channelIndex;
    const time = Number.isFinite(event.peakSampleIndex) && event.sampleRateHz > 0
      ? event.peakSampleIndex * 1000 / event.sampleRateHz : event.elapsedMs;
    if (!Number.isInteger(channel) || channel < 0 || channel >= channelCount || !Number.isFinite(time) || time > endMs) continue;
    if (time >= start) counts[channel]++;
    const age = endMs - time + Math.max(0, wallAgeMs);
    if (age < flashMs) flashes[channel] = Math.max(flashes[channel], 1 - age / flashMs);
  }
  return { counts, ratesHz: counts.map((count) => count * 1000 / windowMs), flashes };
}

export function buildThresholdRaster(events, endElapsedMs, windowMs, channelCount) {
  const end = Math.max(0, finiteNumber(endElapsedMs));
  const duration = Math.max(1, finiteNumber(windowMs, 10_000));
  const start = Math.max(0, end - duration);
  const channels = Math.max(0, Math.floor(finiteNumber(channelCount)));
  const points = [];
  const counts = Array.from({ length: channels }, () => 0);

  for (const event of events || []) {
    const channelIndex = Math.floor(finiteNumber(event?.channelIndex, -1));
    const elapsedMs = finiteNumber(event?.elapsedMs, -1);
    if (event?.category !== "threshold" || channelIndex < 0 || channelIndex >= channels) continue;
    if (elapsedMs < start || elapsedMs > end) continue;
    counts[channelIndex] += 1;
    points.push({
      id: event.id,
      channelIndex,
      sampleIndex: event.sampleIndex ?? null,
      elapsedMs,
      position: Math.min(1, Math.max(0, (elapsedMs - start) / duration))
    });
  }

  return {
    startElapsedMs: start,
    endElapsedMs: end,
    windowMs: duration,
    points,
    counts,
    ratesHz: counts.map((count) => count / (duration / 1000))
  };
}

export function frameChannelSeries(frame, channelIndex) {
  const channelCount = Math.max(0, Math.floor(finiteNumber(frame?.channelCount)));
  const pointCount = Math.max(0, Math.floor(finiteNumber(frame?.pointCount)));
  const channel = Math.floor(finiteNumber(channelIndex, -1));
  const binWidth = Math.max(1, Math.floor(finiteNumber(frame?.binWidthSamples, 1)));
  if (channel < 0 || channel >= channelCount || pointCount === 0) {
    return { samples: [], sampleIndexes: [], sampleRateHz: 0, binWidthSamples: binWidth };
  }
  const samples = [];
  const sampleIndexes = [];
  for (let point = 0; point < pointCount; point += 1) {
    const index = point * channelCount + channel;
    const minimum = finiteNumber(frame.minimums?.[index], 32768);
    const maximum = finiteNumber(frame.maximums?.[index], 32768);
    samples.push((minimum + maximum) / 2 - 32768);
    sampleIndexes.push(Math.floor(finiteNumber(frame.firstSampleIndex)) + point * binWidth);
  }
  return {
    samples,
    sampleIndexes,
    sampleRateHz: finiteNumber(frame.sampleRateHz) / binWidth,
    binWidthSamples: binWidth
  };
}

export function appendSignalHistory(history, frame, maximumPoints = 16384) {
  const channelCount = Math.max(0, Math.floor(finiteNumber(frame?.channelCount)));
  const pointCount = Math.max(0, Math.floor(finiteNumber(frame?.pointCount)));
  if (!channelCount || !pointCount) return history;
  const binWidth = Math.max(1, Math.floor(finiteNumber(frame.binWidthSamples, 1)));
  const effectiveRate = finiteNumber(frame.sampleRateHz) / binWidth;
  const previous = history && history.channels?.length === channelCount ? history : null;
  const next = previous || {
    channels: Array.from({ length: channelCount }, () => []),
    sampleIndexes: [],
    sampleRateHz: effectiveRate,
    sourceSampleRateHz: finiteNumber(frame.sampleRateHz),
    binWidthSamples: binWidth,
    lastSequence: null
  };
  const firstIndex = Math.floor(finiteNumber(frame.firstSampleIndex));
  const lastStoredIndex = next.sampleIndexes.at(-1);
  if (lastStoredIndex !== undefined && (firstIndex !== next.endSampleIndex || binWidth !== next.binWidthSamples
      || finiteNumber(frame.sampleRateHz) !== next.sourceSampleRateHz || (frame.flags & 1))) {
    next.channels = Array.from({ length: channelCount }, () => []);
    next.sampleIndexes = [];
  }
  for (let point = 0; point < pointCount; point += 1) {
    next.sampleIndexes.push(firstIndex + point * binWidth);
    for (let channel = 0; channel < channelCount; channel += 1) {
      const index = point * channelCount + channel;
      const minimum = finiteNumber(frame.minimums?.[index], 32768);
      const maximum = finiteNumber(frame.maximums?.[index], 32768);
      next.channels[channel].push((minimum + maximum) / 2 - 32768);
    }
  }
  const limit = Math.max(256, Math.floor(finiteNumber(maximumPoints, 16384)));
  const excess = Math.max(0, next.sampleIndexes.length - limit);
  if (excess) {
    next.sampleIndexes.splice(0, excess);
    for (const channel of next.channels) channel.splice(0, excess);
  }
  next.sampleRateHz = effectiveRate;
  next.sourceSampleRateHz = finiteNumber(frame.sampleRateHz);
  next.binWidthSamples = binWidth;
  next.lastSequence = finiteNumber(frame.sequence);
  next.endSampleIndex = firstIndex + (frame.sampleCount ?? pointCount * binWidth);
  return next;
}

function largestPowerOfTwo(value) {
  let result = 1;
  while (result * 2 <= value) result *= 2;
  return result;
}

function fftReal(samples) {
  const size = samples.length;
  const real = samples.slice();
  const imaginary = Array(size).fill(0);
  for (let index = 1, reversed = 0; index < size; index += 1) {
    let bit = size >> 1;
    for (; reversed & bit; bit >>= 1) reversed ^= bit;
    reversed ^= bit;
    if (index < reversed) {
      [real[index], real[reversed]] = [real[reversed], real[index]];
      [imaginary[index], imaginary[reversed]] = [imaginary[reversed], imaginary[index]];
    }
  }
  for (let length = 2; length <= size; length *= 2) {
    const angle = -2 * Math.PI / length;
    const baseReal = Math.cos(angle);
    const baseImaginary = Math.sin(angle);
    for (let start = 0; start < size; start += length) {
      let unitReal = 1;
      let unitImaginary = 0;
      for (let offset = 0; offset < length / 2; offset += 1) {
        const even = start + offset;
        const odd = even + length / 2;
        const oddReal = real[odd] * unitReal - imaginary[odd] * unitImaginary;
        const oddImaginary = real[odd] * unitImaginary + imaginary[odd] * unitReal;
        real[odd] = real[even] - oddReal;
        imaginary[odd] = imaginary[even] - oddImaginary;
        real[even] += oddReal;
        imaginary[even] += oddImaginary;
        const nextUnitReal = unitReal * baseReal - unitImaginary * baseImaginary;
        unitImaginary = unitReal * baseImaginary + unitImaginary * baseReal;
        unitReal = nextUnitReal;
      }
    }
  }
  return { real, imaginary };
}

export function powerSpectrum(samples, sampleRateHz, maximumSize = 2048) {
  const input = Array.from(samples || [], (value) => finiteNumber(value));
  const size = Math.min(largestPowerOfTwo(input.length), largestPowerOfTwo(Math.max(8, maximumSize)));
  const rate = finiteNumber(sampleRateHz);
  if (size < 8 || rate <= 0) return { frequencies: [], powerDb: [], size: 0, resolutionHz: 0 };
  const source = input.slice(-size);
  const mean = source.reduce((sum, value) => sum + value, 0) / size;
  const windowed = source.map((value, index) => {
    const hann = 0.5 - 0.5 * Math.cos(2 * Math.PI * index / (size - 1));
    return (value - mean) * hann;
  });
  const transformed = fftReal(windowed);
  const frequencies = [];
  const powerDb = [];
  for (let bin = 0; bin <= size / 2; bin += 1) {
    const magnitudeSquared = transformed.real[bin] ** 2 + transformed.imaginary[bin] ** 2;
    frequencies.push(bin * rate / size);
    powerDb.push(10 * Math.log10(Math.max(1e-12, magnitudeSquared / (size * size))));
  }
  return { frequencies, powerDb, size, resolutionHz: rate / size };
}

export function buildSpectrogram(samples, sampleRateHz, options = {}) {
  const input = Array.from(samples || [], (value) => finiteNumber(value));
  const windowSize = largestPowerOfTwo(Math.max(16, Math.floor(finiteNumber(options.windowSize, 256))));
  const hopSize = Math.max(1, Math.floor(finiteNumber(options.hopSize, windowSize / 2)));
  const maxFrames = Math.max(1, Math.floor(finiteNumber(options.maxFrames, 96)));
  if (input.length < windowSize) return { frames: [], frequencies: [], sampleRateHz };
  const availableFrames = Math.floor((input.length - windowSize) / hopSize) + 1;
  const firstFrame = Math.max(0, availableFrames - maxFrames);
  const frames = [];
  let frequencies = [];
  for (let frame = firstFrame; frame < availableFrames; frame += 1) {
    const start = frame * hopSize;
    const spectrum = powerSpectrum(input.slice(start, start + windowSize), sampleRateHz, windowSize);
    frequencies = spectrum.frequencies;
    frames.push(spectrum.powerDb);
  }
  return { frames, frequencies, sampleRateHz, windowSize, hopSize };
}

export function buildIsiHistogram(events, channelIndex, maximumIntervalMs = 500, binCount = 40) {
  const times = (events || [])
    .filter((event) => event?.category === "threshold" && Number(event.channelIndex) === Number(channelIndex))
    .map((event) => finiteNumber(event.elapsedMs, -1))
    .filter((value) => value >= 0)
    .sort((left, right) => left - right);
  const maximum = Math.max(1, finiteNumber(maximumIntervalMs, 500));
  const bins = Array.from({ length: Math.max(1, Math.floor(binCount)) }, () => 0);
  const intervalsMs = [];
  for (let index = 1; index < times.length; index += 1) {
    const interval = times[index] - times[index - 1];
    if (interval < 0 || interval > maximum) continue;
    intervalsMs.push(interval);
    const bin = Math.min(bins.length - 1, Math.floor(interval / maximum * bins.length));
    bins[bin] += 1;
  }
  return { bins, intervalsMs, maximumIntervalMs: maximum, binWidthMs: maximum / bins.length };
}

export function buildPsth(events, triggerEvents, channelIndex, beforeMs = 500, afterMs = 1000, binMs = 50) {
  const before = Math.max(0, finiteNumber(beforeMs, 500));
  const after = Math.max(1, finiteNumber(afterMs, 1000));
  const width = Math.max(1, finiteNumber(binMs, 50));
  const binCount = Math.ceil((before + after) / width);
  const bins = Array.from({ length: binCount }, () => 0);
  const spikes = (events || [])
    .filter((event) => event?.category === "threshold" && Number(event.channelIndex) === Number(channelIndex))
    .map((event) => finiteNumber(event.elapsedMs, -1))
    .filter((value) => value >= 0);
  const triggers = (triggerEvents || [])
    .map((event) => finiteNumber(event?.elapsedMs, -1))
    .filter((value) => value >= 0);
  for (const trigger of triggers) {
    for (const spike of spikes) {
      const relative = spike - trigger;
      if (relative < -before || relative >= after) continue;
      bins[Math.floor((relative + before) / width)] += 1;
    }
  }
  const divisor = Math.max(1, triggers.length) * (width / 1000);
  return {
    bins,
    ratesHz: bins.map((count) => count / divisor),
    triggerCount: triggers.length,
    beforeMs: before,
    afterMs: after,
    binMs: width
  };
}

export function extractSpikeSnippets(history, events, channelIndex, radiusPoints = 24, maximumSnippets = 12) {
  const indexes = history?.sampleIndexes || [];
  const samples = history?.channels?.[channelIndex] || [];
  if (!indexes.length || samples.length !== indexes.length) return [];
  const targets = (events || [])
    .filter((event) => event?.category === "threshold" && Number(event.channelIndex) === Number(channelIndex) && event.sampleIndex !== null)
    .slice(-Math.max(1, maximumSnippets));
  const radius = Math.max(2, Math.floor(radiusPoints));
  const snippets = [];
  for (const event of targets) {
    const target = Number(event.sampleIndex);
    let low = 0;
    let high = indexes.length - 1;
    while (low < high) {
      const middle = Math.floor((low + high) / 2);
      if (indexes[middle] < target) low = middle + 1;
      else high = middle;
    }
    const center = low > 0 && Math.abs(indexes[low - 1] - target) < Math.abs(indexes[low] - target) ? low - 1 : low;
    if (center - radius < 0 || center + radius >= samples.length) continue;
    const baseline = samples[center - radius];
    snippets.push(samples.slice(center - radius, center + radius + 1).map((value) => value - baseline));
  }
  return snippets;
}
