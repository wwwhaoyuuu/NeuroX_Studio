const COLORS = [
  "#42d3b0", "#69b9ff", "#f3b64b", "#ff7d6e",
  "#9b8cff", "#55c7d9", "#a8d35f", "#f38ab7"
];

const DEFAULT_PAGE_SIZE = 16;
export const RHS_UV_PER_ADC = 0.195;
const ADC_CENTER = 32768;
const LANE_HALF_HEIGHT = 0.45;
const BASE_GAIN_SCALE = 0.38 / 2048;

export function visibleVoltageRangeUv(gain) {
  const value = Math.max(0.05, Number(gain) || 1);
  return LANE_HALF_HEIGHT * RHS_UV_PER_ADC / (BASE_GAIN_SCALE * value);
}

export function normalizeWaveformRange(range) {
  if (!range || typeof range !== "object") return null;
  const { minimumUv, maximumUv } = range;
  if (!Number.isFinite(minimumUv) || !Number.isFinite(maximumUv)
      || minimumUv < -1_000_000 || maximumUv > 1_000_000 || maximumUv - minimumUv < 0.1 - 1e-9) return null;
  return { minimumUv, maximumUv };
}

export function waveformVoltageY(voltageUv, plot, range, clamp = true) {
  const position = (range.maximumUv - voltageUv) / (range.maximumUv - range.minimumUv);
  const fraction = clamp ? Math.min(1, Math.max(0, position)) : position;
  return plot.top + plot.height * (0.05 + 0.9 * fraction);
}

export function waveformAmplitudeStats(frames, channel) {
  let minimum = Infinity;
  let maximum = -Infinity;
  let points = 0;
  for (const frame of frames || []) {
    if (!frame || channel < 0 || channel >= frame.channelCount) continue;
    for (let point = 0; point < frame.pointCount; point += 1) {
      const index = point * frame.channelCount + channel;
      const low = frame.minimums?.[index];
      const high = frame.maximums?.[index];
      if (!Number.isFinite(low) || !Number.isFinite(high)) continue;
      minimum = Math.min(minimum, low);
      maximum = Math.max(maximum, high);
      points += 1;
    }
  }
  if (!points) return null;
  return {
    minimumUv: (minimum - ADC_CENTER) * RHS_UV_PER_ADC,
    maximumUv: (maximum - ADC_CENTER) * RHS_UV_PER_ADC,
    peakToPeakUv: (maximum - minimum) * RHS_UV_PER_ADC,
    touchesRail: minimum <= 0 || maximum >= 65535
  };
}

export function channelPageState(totalChannels, pageIndex, pageSize = DEFAULT_PAGE_SIZE) {
  const total = Math.max(0, Math.floor(Number(totalChannels) || 0));
  const size = Math.max(1, Math.floor(Number(pageSize) || DEFAULT_PAGE_SIZE));
  const pageCount = Math.max(1, Math.ceil(total / size));
  const index = Math.min(pageCount - 1, Math.max(0, Math.floor(Number(pageIndex) || 0)));
  const start = index * size;
  const end = Math.min(total, start + size);
  return {
    totalChannels: total,
    pageIndex: index,
    pageCount,
    pageSize: size,
    start,
    end,
    visibleCount: end - start
  };
}

export function waveformChannelSelection(totalChannels, pageIndex, pageSize, focusChannel = null) {
  const page = channelPageState(totalChannels, pageIndex, pageSize);
  if (focusChannel !== null && Number.isFinite(Number(focusChannel)) && page.totalChannels > 0) {
    return [Math.min(page.totalChannels - 1, Math.max(0, Math.floor(Number(focusChannel))))];
  }
  return Array.from({ length: page.visibleCount }, (_, index) => page.start + index);
}

export function waveformPanes(channels, width, height, columns = 1, labelWidth = 102, focused = false) {
  const count = focused ? 1 : Math.min(columns, Math.max(1, Math.ceil(channels.length / 32)));
  const rows = count > 1 ? 32 : Math.max(1, channels.length);
  return Array.from({ length: count }, (_, column) => {
    const paneWidth = width / count;
    const left = column * paneWidth + labelWidth;
    return { channels: channels.slice(column * rows, (column + 1) * rows), column, rows,
      plot: focused ? { left: left + 65, top: 30, width: Math.max(1, paneWidth - labelWidth - 80), height: Math.max(1, height - 60) }
        : { left, top: 0, width: Math.max(1, paneWidth - labelWidth), height } };
  });
}

export function waveformPointerChannel(panes, x, y) {
  for (const { channels, rows, plot } of panes) {
    if (x < plot.left || x >= plot.left + plot.width || y < plot.top || y >= plot.top + plot.height) continue;
    return channels[Math.floor((y - plot.top) / plot.height * rows)] ?? null;
  }
  return null;
}

export function waveformSampleCount(frame) {
  return frame.sampleCount ?? frame.pointCount * frame.binWidthSamples;
}

export function waveformColumns(frames, channel, startSample, endSample, width) {
  const segments = [];
  let segment = null;
  let expectedSample = null;
  const columns = Math.max(1, Math.floor(width));
  const duration = Math.max(1, endSample - startSample);
  for (const frame of frames) {
    if (channel >= frame.channelCount) { expectedSample = null; continue; }
    const frameEnd = frame.firstSampleIndex + waveformSampleCount(frame);
    if ((frame.flags & 1) || frame.firstSampleIndex !== expectedSample) segment = null;
    for (let point = 0; point < frame.pointCount; point++) {
      const sample = frame.firstSampleIndex + point * frame.binWidthSamples;
      const binEnd = Math.min(frameEnd, sample + frame.binWidthSamples);
      if (binEnd <= startSample || sample >= endSample) continue;
      const index = point * frame.channelCount + channel;
      const minimum = frame.minimums[index];
      const maximum = frame.maximums[index];
      if (!Number.isFinite(minimum) || !Number.isFinite(maximum)) { segment = null; continue; }
      const column = Math.min(columns - 1, Math.max(0, Math.floor((sample - startSample) / duration * columns)));
      const first = frame.firstValues?.[index] ?? (minimum + maximum) / 2;
      const last = frame.lastValues?.[index] ?? (minimum + maximum) / 2;
      if (!segment) { segment = []; segments.push(segment); }
      const previous = segment.at(-1);
      if (previous?.column === column) {
        previous.minimum = Math.min(previous.minimum, minimum);
        previous.maximum = Math.max(previous.maximum, maximum);
        previous.last = last;
        previous.endSample = Math.min(endSample, binEnd);
      } else {
        segment.push({ column, minimum, maximum, first, last,
          startSample: Math.max(startSample, sample), endSample: Math.min(endSample, binEnd) });
      }
    }
    expectedSample = frameEnd;
  }
  return segments;
}

export function waveformSpikeMarkers(frames, events, options) {
  const { epoch, channels, startSample, endSample, plot, gain, focusRange, channelRanges, rows = channels.length } = options;
  const lanes = new Map(channels.map((channel, lane) => [channel, lane]));
  const laneHeight = plot.height / Math.max(1, rows);
  const result = [];
  for (const event of events || []) {
    const sample = event.peakSampleIndex, channel = event.channelIndex;
    if (!Number.isSafeInteger(sample) || !Number.isInteger(channel) || !lanes.has(channel)
        || sample < startSample || sample >= endSample || !Number.isFinite(event.displayPeakUv)) continue;
    const frame = frames.find((frame) => frame.spikeEpoch === epoch
      && frame.sampleRateHz === event.sampleRateHz && channel < frame.channelCount
      && sample >= frame.firstSampleIndex && sample < frame.firstSampleIndex + waveformSampleCount(frame));
    if (!frame || (frame.flags & 2)) continue;
    const center = plot.top + laneHeight * (lanes.get(channel) + 0.5);
    const range = focusRange || channelRanges?.[channel];
    const lanePlot = focusRange ? plot : { top: center - laneHeight / 2, height: laneHeight };
    const y = range ? waveformVoltageY(event.displayPeakUv, lanePlot, range, false)
      : center - event.displayPeakUv / RHS_UV_PER_ADC * laneHeight * BASE_GAIN_SCALE * gain;
    const top = center - laneHeight * LANE_HALF_HEIGHT;
    const bottom = center + laneHeight * LANE_HALF_HEIGHT;
    if (y < top || y > bottom) continue;
    result.push({ event, x: plot.left + (sample - startSample) / (endSample - startSample) * plot.width, y });
  }
  return result;
}

export function nearestSpikeMarker(markers, x, y, radius = 9) {
  let nearest = null, distance = radius * radius;
  for (const marker of markers) {
    const squared = (marker.x - x) ** 2 + (marker.y - y) ** 2;
    if (squared <= distance) { nearest = marker; distance = squared; }
  }
  return nearest;
}

export class WaveformRenderer {
  constructor(canvas, labels, pagination = {}) {
    this.canvas = canvas;
    this.context = canvas.getContext("2d", { alpha: false });
    this.labels = labels;
    this.pager = pagination.pager || null;
    this.previousPageButton = pagination.previousPageButton || null;
    this.nextPageButton = pagination.nextPageButton || null;
    this.pageLabel = pagination.pageLabel || null;
    this.pageSize = Math.max(1, Number(pagination.pageSize) || DEFAULT_PAGE_SIZE);
    this.columns = this.pageSize === 64 ? 2 : 1;
    this.labelWidth = 102;
    this.pageIndex = 0;
    this.focusChannel = null;
    this.focusRange = null;
    this.channelRanges = [];
    this.onFocusChannel = pagination.onFocusChannel || null;
    this.spikeTooltip = pagination.spikeTooltip || null;
    this.onSpikeMarkers = pagination.onSpikeMarkers || null;
    this.spikeEvents = [];
    this.spikeEpoch = null;
    this.spikeCursor = null;
    this.showSpikeMarkers = true;
    this.markerPoints = [];
    this.channelCount = DEFAULT_PAGE_SIZE;
    this.frame = null;
    this.frames = [];
    this.timeWindowMs = 250;
    this.gain = 1;
    this.paused = false;
    this.channelNames = [];
    this.channelColors = [];
    this.visibleChannels = Array.from({ length: this.channelCount }, () => true);
    this.pixelRatio = Math.min(window.devicePixelRatio || 1, 2);
    this.resizeObserver = new ResizeObserver(() => this.resize());
    this.resizeObserver.observe(canvas.parentElement);
    this.previousPageButton?.addEventListener("click", () => this.setPage(this.pageIndex - 1));
    this.nextPageButton?.addEventListener("click", () => this.setPage(this.pageIndex + 1));
    this.canvas.addEventListener("click", (event) => this.focusAtPointer(event));
    this.canvas.addEventListener("pointermove", (event) => { this.spikePointer = event; this.inspectSpike(event); });
    this.canvas.addEventListener("pointerleave", () => { this.spikePointer = null; this.hideSpikeTooltip(); });
    this.buildLabels();
    this.resize();
  }

  pageState() {
    return channelPageState(this.channelCount, this.pageIndex, this.pageSize);
  }

  displayChannels() {
    return waveformChannelSelection(this.channelCount, this.pageIndex, this.pageSize, this.focusChannel);
  }

  panes(width = this.canvas.width / this.pixelRatio, height = this.canvas.height / this.pixelRatio) {
    return waveformPanes(this.displayChannels(), width, height, this.columns, this.labelWidth, this.focusChannel !== null);
  }

  setLayout(layout) {
    const anchor = this.pageIndex * this.pageSize;
    this.columns = layout === "dual-64" ? 2 : 1;
    this.pageSize = this.columns === 2 ? 64 : 16;
    this.pageIndex = channelPageState(this.channelCount, Math.floor(anchor / this.pageSize), this.pageSize).pageIndex;
    this.buildLabels(); this.resize();
  }

  setChannelRanges(ranges) {
    const next = ranges.map(normalizeWaveformRange);
    if (JSON.stringify(this.channelRanges) === JSON.stringify(next)) return;
    this.channelRanges = next;
    this.buildLabels(); this.draw();
  }

  focusAtPointer(event) {
    if (this.focusChannel !== null) return;
    const rect = this.canvas.getBoundingClientRect();
    const channel = waveformPointerChannel(this.panes(rect.width, rect.height), event.clientX - rect.left, event.clientY - rect.top);
    if (channel !== null) this.onFocusChannel?.(channel);
  }

  setFocusChannel(channel) {
    this.focusChannel = channel === null ? null : waveformChannelSelection(this.channelCount, this.pageIndex, this.pageSize, channel)[0] ?? null;
    this.buildLabels();
    this.draw();
  }

  setFocusRange(range) {
    this.focusRange = normalizeWaveformRange(range);
    this.draw();
  }

  voltageRange(channel = this.focusChannel) {
    if (channel !== null && this.channelRanges?.[channel]) return this.channelRanges[channel];
    if (channel !== null && channel === this.focusChannel && this.focusRange) return this.focusRange;
    const range = visibleVoltageRangeUv(this.gain);
    return { minimumUv: -range, maximumUv: range };
  }

  setPage(pageIndex) {
    const page = channelPageState(this.channelCount, pageIndex, this.pageSize);
    if (page.pageIndex === this.pageIndex) return;
    this.pageIndex = page.pageIndex;
    this.buildLabels();
    this.draw();
  }

  syncChannelCount(count, rebuildLabels = false) {
    const nextCount = Math.max(1, Math.floor(Number(count) || DEFAULT_PAGE_SIZE));
    const changed = nextCount !== this.channelCount;
    if (changed) {
      const previous = this.visibleChannels;
      this.channelCount = nextCount;
      this.visibleChannels = Array.from({ length: nextCount }, (_, index) => previous[index] ?? true);
      this.pageIndex = channelPageState(nextCount, this.pageIndex, this.pageSize).pageIndex;
      if (this.focusChannel !== null) this.focusChannel = Math.min(nextCount - 1, this.focusChannel);
    }
    if (changed || rebuildLabels) this.buildLabels();
  }

  buildLabels() {
    const page = this.pageState();
    const channels = this.displayChannels();
    const panes = waveformPanes(channels, 1000, 1000, this.columns, this.labelWidth, this.focusChannel !== null);
    this.labels.replaceChildren();
    this.labels.parentElement?.style.setProperty("--visible-channel-count", String(panes[0].rows));
    this.labels.parentElement?.style.setProperty("--waveform-columns", String(panes.length));
    this.labels.style.setProperty("--visible-channel-count", String(panes[0].rows));
    this.labels.style.setProperty("--waveform-columns", String(panes.length));
    for (const [index, channel] of channels.entries()) {
      const row = document.createElement("div");
      row.className = "channel-row";
      row.style.gridColumn = String(Math.floor(index / panes[0].rows) + 1);
      row.style.gridRow = String(index % panes[0].rows + 1);
      const button = document.createElement("button");
      button.className = `channel-label ${this.visibleChannels[channel] ? "active" : ""}`;
      button.dataset.channel = String(channel);
      const channelName = this.channelNames[channel] || `CH-${String(channel + 1).padStart(2, "0")}`;
      button.title = this.focusChannel === null ? `查看 ${channelName} 通道详情` : `${channelName} · 单通道`;
      const range = this.channelRanges?.[channel];
      if (range) button.title += ` · ${range.minimumUv} ～ ${range.maximumUv} µV`;
      row.classList.toggle("has-custom-range", Boolean(range));
      const channelColor = this.channelColors[channel] || COLORS[channel % COLORS.length];
      const dot = document.createElement("span"); dot.style.setProperty("--channel-color", channelColor);
      const name = document.createElement("strong"); name.textContent = channelName;
      button.append(dot, name);
      button.addEventListener("click", () => this.onFocusChannel?.(channel));
      const visibility = document.createElement("input");
      visibility.type = "checkbox";
      visibility.checked = this.visibleChannels[channel];
      visibility.hidden = this.focusChannel !== null;
      visibility.title = `显示 ${channelName}`;
      visibility.setAttribute("aria-label", `显示 ${channelName}`);
      visibility.addEventListener("change", () => {
        this.visibleChannels[channel] = visibility.checked;
        button.classList.toggle("active", visibility.checked);
        this.draw();
      });
      row.append(button, visibility);
      this.labels.appendChild(row);
    }
    if (this.pager) this.pager.hidden = this.focusChannel !== null || page.pageCount <= 1;
    if (this.pageLabel) {
      const first = page.visibleCount ? page.start + 1 : 0;
      this.pageLabel.textContent = `${first}–${page.end} / ${page.totalChannels}`;
      this.pageLabel.title = `第 ${page.pageIndex + 1} 页，共 ${page.pageCount} 页`;
    }
    if (this.previousPageButton) this.previousPageButton.disabled = page.pageIndex === 0;
    if (this.nextPageButton) this.nextPageButton.disabled = page.pageIndex >= page.pageCount - 1;
  }

  setGain(value) {
    this.gain = Math.min(4, Math.max(0.05, Number(value) || 1));
    this.draw();
  }

  displayFrames() {
    if (!this.frame) return [];
    const end = this.frame.firstSampleIndex + waveformSampleCount(this.frame);
    const start = end - this.frame.sampleRateHz * this.timeWindowMs / 1000;
    return this.frames.filter((frame) => frame.firstSampleIndex < end
      && frame.firstSampleIndex + waveformSampleCount(frame) > start);
  }

  amplitudeStats(channel) {
    return waveformAmplitudeStats(this.displayFrames(), channel);
  }

  fitGain() {
    const frames = this.displayFrames();
    let maximumUv = 0;
    for (const channel of this.displayChannels()) {
      if (this.focusChannel === null && !this.visibleChannels[channel]) continue;
      const stats = waveformAmplitudeStats(frames, channel);
      if (stats) maximumUv = Math.max(maximumUv, Math.abs(stats.minimumUv), Math.abs(stats.maximumUv));
    }
    if (!maximumUv) return 1;
    const gain = LANE_HALF_HEIGHT * RHS_UV_PER_ADC / (BASE_GAIN_SCALE * maximumUv * 1.15);
    return Math.max(0.05, Math.min(4, Math.floor(gain * 20) / 20));
  }

  setTimeWindow(value) {
    this.timeWindowMs = Math.min(1000, Math.max(100, Number(value) || 250));
    this.draw();
  }

  setPaused(paused) {
    this.paused = paused;
  }

  setSpikeEvents(history) {
    if (this.paused || (this.spikeEpoch === history.epoch && this.spikeCursor === history.afterId)) return;
    this.spikeEpoch = history.epoch;
    this.spikeCursor = history.afterId;
    const first = this.frames[0]?.firstSampleIndex ?? 0;
    this.spikeEvents = history.events.filter((event) => event.peakSampleIndex >= first).map((event) => ({
      id: event.id, channelIndex: event.channelIndex, peakSampleIndex: event.peakSampleIndex,
      sampleRateHz: event.sampleRateHz, peakUv: event.peakUv, displayPeakUv: event.displayPeakUv,
      thresholdUv: event.thresholdUv, noiseUv: event.noiseUv
    }));
    this.draw();
  }

  setSpikeMarkersVisible(visible) {
    this.showSpikeMarkers = Boolean(visible);
    this.draw();
  }

  setDetectionThresholds(batch) {
    if (!this.paused) this.detection = batch;
  }

  hideSpikeTooltip() {
    if (this.spikeTooltip) this.spikeTooltip.hidden = true;
  }

  inspectSpike(pointer) {
    if (!this.spikeTooltip) return;
    const rect = this.canvas.getBoundingClientRect();
    const marker = nearestSpikeMarker(this.markerPoints, pointer.clientX - rect.left, pointer.clientY - rect.top);
    if (!marker) { this.hideSpikeTooltip(); return; }
    const event = marker.event;
    const name = this.channelNames[event.channelIndex] || `CH-${String(event.channelIndex + 1).padStart(2, "0")}`;
    this.spikeTooltip.textContent = `${name} · ${(event.peakSampleIndex / event.sampleRateHz).toFixed(6)} s\n检测峰值 ${event.peakUv.toFixed(2)} µV\n当前波形 ${event.displayPeakUv.toFixed(2)} µV`
      + (Number.isFinite(event.thresholdUv) ? `\n阈值 ${event.thresholdUv.toFixed(2)} µV` : "")
      + (Number.isFinite(event.noiseUv) ? ` · σ ${event.noiseUv.toFixed(2)} µV` : "");
    this.spikeTooltip.style.left = `${Math.max(8, Math.min(rect.width - 270, marker.x + 12))}px`;
    this.spikeTooltip.style.top = `${Math.max(8, Math.min(rect.height - 124, marker.y + 12))}px`;
    this.spikeTooltip.hidden = false;
  }

  setChannelNames(names) {
    this.channelNames = Array.isArray(names) ? names.slice() : [];
    this.syncChannelCount(Math.max(this.channelNames.length, this.frame?.channelCount || 0), true);
  }

  setChannelColors(colors) {
    this.channelColors = Array.isArray(colors) ? colors.slice() : [];
    this.buildLabels();
    this.draw();
  }

  setChannelEnabled(enabled) {
    if (!Array.isArray(enabled)) return;
    this.syncChannelCount(Math.max(this.channelCount, enabled.length));
    this.visibleChannels = Array.from({ length: this.channelCount }, (_, index) => enabled[index] !== false);
    this.buildLabels();
    this.draw();
  }

  clearFrames() {
    this.frame = null;
    this.frames = [];
    this.spikeEvents = [];
    this.spikeCursor = null;
    this.draw();
  }

  setFrames(frames) {
    for (const frame of frames) this.setFrame(frame, false);
    this.draw();
  }

  setFrame(frame, redraw = true) {
    if (!frame || this.paused) return;
    if (this.frame?.sequence === frame.sequence && this.frame.firstSampleIndex === frame.firstSampleIndex && this.frame.channelCount === frame.channelCount) return;
    if (this.frame && (this.frame.channelCount !== frame.channelCount || this.frame.sampleRateHz !== frame.sampleRateHz || frame.firstSampleIndex < this.frame.firstSampleIndex)) this.frames = [];
    this.frame = frame;
    this.frames.push(frame);

    const maximumSamples = frame.sampleRateHz * 1.2;
    const end = frame.firstSampleIndex + waveformSampleCount(frame);
    while (this.frames.length > 1 && (this.frames.length > 512
      || end - (this.frames[0].firstSampleIndex + waveformSampleCount(this.frames[0])) > maximumSamples)) this.frames.shift();

    this.syncChannelCount(Math.max(this.channelNames.length, frame.channelCount));
    if (redraw) this.draw();
  }

  resize() {
    const rect = this.canvas.parentElement.getBoundingClientRect();
    const width = Math.max(1, Math.floor(rect.width));
    const height = Math.max(1, Math.floor(rect.height));
    this.canvas.width = Math.floor(width * this.pixelRatio);
    this.canvas.height = Math.floor(height * this.pixelRatio);
    this.canvas.style.width = `${width}px`;
    this.canvas.style.height = `${height}px`;
    this.context.setTransform(this.pixelRatio, 0, 0, this.pixelRatio, 0, 0);
    this.draw();
  }

  draw() {
    this.hideSpikeTooltip?.();
    this.markerPoints = [];
    const context = this.context;
    const width = this.canvas.width / this.pixelRatio;
    const height = this.canvas.height / this.pixelRatio;
    context.fillStyle = "#111719";
    context.fillRect(0, 0, width, height);
    const channels = this.displayChannels();
    const focused = this.focusChannel !== null;
    const panes = this.panes(width, height);
    for (const { plot, rows } of panes) {
      if (focused) this.drawFocusGrid(context, plot);
      else this.drawGrid(context, plot, rows);
    }
    if (!this.frame) { this.onSpikeMarkers?.(0); return; }

    const dataChannelCount = this.frame.channelCount;
    const displayFrames = this.displayFrames();

    const endSample = this.frame.firstSampleIndex + waveformSampleCount(this.frame);
    const windowSamples = this.frame.sampleRateHz * this.timeWindowMs / 1000;
    const startSample = endSample - windowSamples;
    for (const { plot, rows, channels: paneChannels } of panes) {
    const laneHeight = plot.height / rows;
    for (const [lane, channel] of paneChannels.entries()) {
      if (channel >= dataChannelCount) continue;
      if (!focused && !this.visibleChannels[channel]) continue;
      const centerY = plot.top + laneHeight * (lane + 0.5);
      const scale = laneHeight * BASE_GAIN_SCALE * this.gain;
      const manualRange = this.channelRanges?.[channel] || (focused && this.focusRange);
      const lanePlot = { top: plot.top + laneHeight * lane, height: laneHeight };
      if (manualRange) {
        context.save(); context.beginPath();
        context.rect(plot.left, lanePlot.top + laneHeight * 0.05, plot.width, laneHeight * 0.9);
        context.clip();
      }
      context.strokeStyle = this.channelColors[channel] || COLORS[channel % COLORS.length];
      context.globalAlpha = 0.86;
      context.lineWidth = 1 / this.pixelRatio;
      context.lineCap = "butt";
      context.beginPath();
      const xAt = (sample) => plot.left + (sample - startSample) / windowSamples * plot.width;
      const yAt = (value) => manualRange
        ? waveformVoltageY((value - ADC_CENTER) * RHS_UV_PER_ADC, lanePlot, manualRange, false)
        : Math.max(centerY - laneHeight * LANE_HALF_HEIGHT,
          Math.min(centerY + laneHeight * LANE_HALF_HEIGHT, centerY - (value - ADC_CENTER) * scale));
      // One envelope per pixel column; join only genuinely contiguous samples.
      for (const segment of waveformColumns(displayFrames, channel, startSample, endSample, plot.width)) {
        let previous = null;
        for (const column of segment) {
          const x = xAt(column.startSample);
          const lastX = xAt(Math.max(column.startSample, column.endSample - 1));
          const middleX = (x + lastX) / 2;
          context.moveTo(middleX, yAt(column.minimum));
          context.lineTo(middleX + (column.minimum === column.maximum ? 0.5 : 0), yAt(column.maximum));
          if (previous) {
            context.moveTo(previous.x, yAt(previous.value));
            context.lineTo(x, yAt(column.first));
          }
          if (lastX > x) {
            context.moveTo(x, yAt(column.first));
            context.lineTo(lastX, yAt(column.last));
          }
          previous = { x: lastX, value: column.last };
        }
      }
      context.stroke();
      if (manualRange) context.restore();
    }
    }
    const plot = panes[0].plot;
    context.globalAlpha = 1;
    const threshold = this.detection?.thresholds?.[this.focusChannel]?.thresholdUv;
    if (focused && this.showSpikeMarkers && this.detection?.config.enabled && !this.detection.config.bandPass
      && this.detection.epoch === this.frame.spikeEpoch && Number.isFinite(threshold)) {
      const range = this.voltageRange();
      const y = waveformVoltageY(threshold, plot, range, false);
      if (threshold >= range.minimumUv && threshold <= range.maximumUv) {
        context.setLineDash([5, 4]); context.strokeStyle = "#ff6a72"; context.lineWidth = 1;
        context.beginPath(); context.moveTo(plot.left, y); context.lineTo(plot.left + plot.width, y); context.stroke(); context.setLineDash([]);
      }
    }
    if (this.showSpikeMarkers && this.spikeEvents?.length) {
      const visible = channels.filter((channel) => focused || this.visibleChannels[channel]);
      // Keep lane indexes stable when overview channels are hidden.
      this.markerPoints = panes.flatMap(({ channels: paneChannels, plot, rows }) => waveformSpikeMarkers(displayFrames, this.spikeEvents, {
        epoch: this.spikeEpoch, channels: paneChannels, startSample, endSample, plot, rows,
        gain: this.gain, focusRange: focused ? this.channelRanges?.[this.focusChannel] || this.focusRange : null,
        channelRanges: this.channelRanges
      })).filter(({ event }) => visible.includes(event.channelIndex));
      context.fillStyle = "#ff4d5e";
      for (const marker of this.markerPoints) {
        context.beginPath(); context.arc(marker.x, marker.y, focused ? 3.5 : 2.7, 0, Math.PI * 2); context.fill();
      }
    }
    this.onSpikeMarkers?.(this.markerPoints.length);
    if (this.spikePointer) this.inspectSpike(this.spikePointer);
  }

  drawFocusGrid(context, plot) {
    const range = this.voltageRange();
    context.font = '14px "Segoe UI", sans-serif';
    context.fillStyle = "#a2b7bd";
    context.textAlign = "right";
    context.fillText("µV", plot.left - 9, plot.top - 8);
    for (let index = 0; index <= 4; index++) {
      const value = range.minimumUv + (range.maximumUv - range.minimumUv) * index / 4;
      const y = waveformVoltageY(value, plot, range);
      context.strokeStyle = Math.abs(value) < 1e-9 ? "#405b63" : "#27383d";
      context.beginPath(); context.moveTo(plot.left, y); context.lineTo(plot.left + plot.width, y); context.stroke();
      context.fillText(Math.abs(value) >= 10000 ? value.toExponential(1) : String(Number(value.toFixed(2))), plot.left - 9, y + 4);
    }
    for (let tick = 0; tick <= 4; tick++) {
      const x = plot.left + plot.width * tick / 4;
      context.strokeStyle = "#27383d";
      context.beginPath(); context.moveTo(x, plot.top); context.lineTo(x, plot.top + plot.height); context.stroke();
      context.textAlign = tick === 0 ? "left" : tick === 4 ? "right" : "center";
      context.fillText(`${Math.round(-this.timeWindowMs * (1 - tick / 4))} ms`, x, plot.top + plot.height + 22);
    }
  }

  drawGrid(context, plot, laneCount) {
    const { left, top, width, height } = plot;
    context.lineWidth = 1;
    context.strokeStyle = "rgba(213, 228, 230, 0.075)";
    context.beginPath();
    for (let x = 0; x <= width; x += 50) {
      context.moveTo(left + x + 0.5, top);
      context.lineTo(left + x + 0.5, top + height);
    }
    for (let y = 0; y <= height; y += height / laneCount) {
      context.moveTo(left, top + y + 0.5);
      context.lineTo(left + width, top + y + 0.5);
    }
    context.stroke();
    context.strokeStyle = "rgba(213, 228, 230, 0.13)";
    context.beginPath();
    for (let x = 0; x <= width; x += 250) {
      context.moveTo(left + x + 0.5, top);
      context.lineTo(left + x + 0.5, top + height);
    }
    context.stroke();
  }
}
