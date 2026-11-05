import { buildThresholdRaster } from "./analysis.js";

export class SpikeHistory {
  constructor(limit = 20000) { this.limit = limit; this.epoch = 0; this.afterId = 0; this.events = []; this.missed = 0; this.evicted = 0; this.batch = null; }
  append(batch) {
    if (batch.epoch !== this.epoch) {
      this.events = []; this.afterId = 0; this.missed = 0; this.evicted = 0; this.epoch = batch.epoch;
    }
    this.missed += Number(batch.missedEvents) || 0;
    for (const event of batch.events || []) {
      if (event.id <= this.afterId) continue;
      this.events.push(event); this.afterId = event.id;
    }
    if (this.events.length > this.limit) {
      this.evicted += this.events.length - this.limit;
      this.events.splice(0, this.events.length - this.limit);
    }
    this.batch = batch;
  }
  analysisEvents() {
    return this.events.map((event) => ({ ...event, category: "threshold", elapsedMs: event.timeMs }));
  }
  snippets(channel, limit = 8) { return this.events.filter((event) => event.channelIndex === channel).slice(-limit); }
}

export function spikeCsv(events) {
  const rows = [["id", "channel_index", "stream", "chip_channel", "crossing_sample", "time_ms", "peak_sample", "peak_uv", "peak_to_peak_uv", "sample_rate_hz", "pre_samples", "waveform_uv", "threshold_uv", "noise_sigma_uv"]];
  for (const event of events) rows.push([event.id, event.channelIndex, event.stream, event.chipChannel, event.sampleIndex, event.timeMs, event.peakSampleIndex, event.peakUv, event.peakToPeakUv, event.sampleRateHz, event.preSamples, event.waveformUv.join(";"), event.thresholdUv ?? "", event.noiseUv ?? ""]);
  return rows.map((row) => row.join(",")).join("\r\n");
}

export function drawSpikeWaveforms(canvas, events, fixedScale = 0, detection = null) {
  const rect = canvas.getBoundingClientRect();
  if (!rect.width || !rect.height) return;
  const dpr = Math.min(2, window.devicePixelRatio || 1);
  canvas.width = Math.round(rect.width * dpr); canvas.height = Math.round(rect.height * dpr);
  const ctx = canvas.getContext("2d"); ctx.scale(dpr, dpr);
  const width = rect.width, height = rect.height;
  const top = 18, bottom = height - 20, left = 38, right = width - 8;
  const scale = fixedScale || Math.max(50, Math.abs(detection?.thresholdUv || 0), ...events.map((event) => Math.max(...event.waveformUv.map(Math.abs)))) * 1.1;
  const center = (top + bottom) / 2;
  ctx.strokeStyle = "#29373b"; ctx.lineWidth = 1;
  for (const y of [top, center, bottom]) { ctx.beginPath(); ctx.moveTo(left, y); ctx.lineTo(right, y); ctx.stroke(); }
  if (Number.isFinite(detection?.thresholdUv) && Math.abs(detection.thresholdUv) <= scale) {
    const y = center - detection.thresholdUv / scale * (bottom - top) / 2;
    ctx.setLineDash([5, 4]); ctx.strokeStyle = "#ff6a72";
    ctx.beginPath(); ctx.moveTo(left, y); ctx.lineTo(right, y); ctx.stroke(); ctx.setLineDash([]);
  }
  const reference = events.at(-1);
  if (reference) {
    const zero = left + reference.preSamples / (reference.waveformUv.length - 1) * (right - left);
    ctx.setLineDash([3, 3]); ctx.strokeStyle = "#c7a04b";
    ctx.beginPath(); ctx.moveTo(zero, top); ctx.lineTo(zero, bottom); ctx.stroke(); ctx.setLineDash([]);
  }
  events.forEach((event, index) => {
    ctx.strokeStyle = index === events.length - 1 ? "#69dfc5" : "#438eaa";
    ctx.globalAlpha = index === events.length - 1 ? 1 : 0.45;
    ctx.beginPath();
    event.waveformUv.forEach((value, point) => {
      const x = left + point / (event.waveformUv.length - 1) * (right - left);
      const y = center - value / scale * (bottom - top) / 2;
      if (point) ctx.lineTo(x, y); else ctx.moveTo(x, y);
    });
    ctx.stroke();
    const peakPoint = event.preSamples + event.peakSampleIndex - event.sampleIndex;
    if (peakPoint >= 0 && peakPoint < event.waveformUv.length && Math.abs(event.peakUv) <= scale) {
      ctx.fillStyle = "#ff4d5e"; ctx.beginPath();
      ctx.arc(left + peakPoint / (event.waveformUv.length - 1) * (right - left), center - event.peakUv / scale * (bottom - top) / 2, 2.5, 0, Math.PI * 2); ctx.fill();
    }
  });
  ctx.globalAlpha = 1; ctx.fillStyle = "#94a8ad"; ctx.font = '11px "Segoe UI", sans-serif';
  ctx.fillText(`${scale.toFixed(0)}`, 2, top + 3); ctx.fillText("µV", 2, center + 4); ctx.fillText(`−${scale.toFixed(0)}`, 2, bottom + 3);
  if (reference) {
    ctx.fillText(`−${(reference.preSamples * 1000 / reference.sampleRateHz).toFixed(1)} ms`, left, height - 4);
    ctx.textAlign = "right";
    ctx.fillText(`+${((reference.waveformUv.length - 1 - reference.preSamples) * 1000 / reference.sampleRateHz).toFixed(1)} ms`, right, height - 4);
  } else { ctx.fillText("暂无候选尖峰", left + 4, center - 8); }
}

export function createSpikePanel({ bridge, profile, status, notify, onUpdate }) {
  const history = new SpikeHistory();
  const $ = (id) => document.getElementById(id);
  let polling = false, initialized = false, dirty = false;
  function busy() { return ["acquiring", "recording", "connecting", "playback", "playback-paused"].includes(status()?.mode); }
  function label(channel) { return profile().channels[channel]?.label || `CH-${String(channel + 1).padStart(2, "0")}`; }
  function download() {
    const url = URL.createObjectURL(new Blob(["\ufeff", spikeCsv(history.events)], { type: "text/csv;charset=utf-8" }));
    const link = document.createElement("a"); link.href = url; link.download = "spikes-buffer.csv"; link.click();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  }
  function bind() {
    $("spike-record-path").value = bridge.defaultRecordingPath().replace(/\.rhsraw$/, "-spikes.csv");
    for (const input of $("spike-config").querySelectorAll("input, select")) input.addEventListener("input", () => { dirty = true; });
    $("spike-threshold-mode").addEventListener("change", () => { dirty = true; render(); });
    $("spike-apply").addEventListener("click", async () => {
      const config = {
        enabled: $("spike-enabled").checked,
        thresholdMode: $("spike-threshold-mode").value,
        noiseMultiplier: Number($("spike-noise-multiplier").value), calibrationMs: Math.round(Number($("spike-calibration").value) * 1000),
        thresholdUv: $("spike-threshold-mode").value === "noise" ? Number($("spike-polarity").value) * Math.max(1, Math.abs(Number($("spike-threshold").value))) : Number($("spike-threshold").value),
        refractoryMs: Number($("spike-refractory").value), preMs: Number($("spike-pre").value), postMs: Number($("spike-post").value),
        bandPass: $("spike-band-pass").checked, recordingPath: $("spike-record").checked ? $("spike-record-path").value.trim() : null
      };
      try {
        await bridge.invoke("configure_spikes", { config }); dirty = false;
        notify("尖峰设置已应用，下次采集或回放生效", "success"); await poll();
      } catch (error) { notify(String(error), "error"); }
    });
    for (const id of ["spike-display", "spike-window", "spike-scale", "spike-channel-filter"]) $(id).addEventListener("change", render);
    $("spike-export").addEventListener("click", download);
  }
  async function poll() {
    if (polling) return;
    polling = true;
    try {
      const batch = await bridge.invoke("poll_spikes", { epoch: history.epoch, afterId: history.afterId });
      history.append(batch);
      if (!initialized || !dirty) {
        const config = batch.config;
        $("spike-enabled").checked = config.enabled; $("spike-threshold").value = config.thresholdUv;
        $("spike-threshold-mode").value = config.thresholdMode || "fixed";
        $("spike-polarity").value = config.thresholdUv < 0 ? "-1" : "1";
        $("spike-noise-multiplier").value = config.noiseMultiplier ?? 5;
        $("spike-calibration").value = (config.calibrationMs ?? 1000) / 1000;
        $("spike-refractory").value = config.refractoryMs; $("spike-pre").value = config.preMs; $("spike-post").value = config.postMs;
        $("spike-band-pass").checked = config.bandPass; $("spike-record").checked = Boolean(config.recordingPath);
        if (config.recordingPath) $("spike-record-path").value = config.recordingPath;
        initialized = true;
      }
      render(); onUpdate?.();
    } catch (error) { $("spike-record-status").textContent = `尖峰数据读取失败：${String(error)}`; }
    finally { polling = false; }
  }
  function render() {
    const batch = history.batch;
    for (const input of $("spike-config").querySelectorAll("input, select, button")) input.disabled = busy();
    const noiseMode = $("spike-threshold-mode").value === "noise";
    $("spike-threshold").disabled = busy() || noiseMode;
    for (const id of ["spike-polarity", "spike-noise-multiplier", "spike-calibration"]) $(id).disabled = busy() || !noiseMode;
    if (!batch) return;
    const thresholds = batch.thresholds || [];
    const calibrating = batch.running && thresholds.some((entry) => entry.state === "calibrating");
    $("spike-detector-state").textContent = calibrating ? "噪声校准中" : batch.running ? "全通道检测中" : batch.config.enabled ? "检测就绪 / 已停止" : "检测未启用";
    const ready = thresholds.filter((entry) => Number.isFinite(entry.thresholdUv));
    $("spike-threshold-summary").textContent = !batch.config.enabled ? "未开启" : batch.config.thresholdMode === "noise" && !thresholds.length ? "等待噪声校准" : calibrating ? `校准 ${batch.config.calibrationMs / 1000} s`
      : batch.config.thresholdMode === "noise" ? `${batch.config.noiseMultiplier}×σ · ${ready.length}/${thresholds.length} 路` : `${batch.config.thresholdUv} µV`;
    $("spike-detector-state").classList.toggle("active", batch.running);
    const counts = batch.counts || [];
    const channelCount = counts.length || status()?.playback?.channelCount || status()?.device?.capabilities?.acquisitionChannelLimit || status()?.device?.channels || profile().channelCount;
    const signature = profile().channels.slice(0, channelCount).map((channel) => channel.label).join("|");
    const selector = $("spike-channel-filter");
    if (selector.dataset.channels !== signature) {
      const previous = selector.value;
      selector.replaceChildren();
      const all = document.createElement("option"); all.value = "all"; all.textContent = "全部通道"; selector.appendChild(all);
      for (let channel = 0; channel < channelCount; channel++) {
        const option = document.createElement("option"); option.value = String(channel); option.textContent = label(channel); selector.appendChild(option);
      }
      selector.dataset.channels = signature;
      selector.value = previous !== "all" && Number(previous) < channelCount ? previous : "all";
    }
    const selectedChannel = selector.value === "all" ? null : Number(selector.value);
    const visibleChannels = selectedChannel === null ? Array.from({ length: channelCount }, (_, index) => index) : [selectedChannel];
    const endMs = batch.latestSampleIndex * 1000 / (batch.sampleRateHz || 30000);
    const raster = buildThresholdRaster(history.analysisEvents(), endMs, Number($("spike-window").value), channelCount);
    $("spike-event-count").textContent = `${counts.reduce((sum, count) => sum + count, 0)} / ${raster.points.length}`;
    $("spike-active-channels").textContent = `${counts.filter((count) => count > 0).length} / ${channelCount}`;
    $("spike-peak-rate").textContent = `${Math.max(0, ...raster.ratesHz).toFixed(2)} Hz`;
    $("spike-latest-sample").textContent = String(batch.latestSampleIndex);
    $("spike-axis-start").textContent = `${(raster.startElapsedMs / 1000).toFixed(2)} s`;
    $("spike-axis-end").textContent = `${(endMs / 1000).toFixed(2)} s`;
    $("spike-export").disabled = !history.events.length;
    $("spike-record-status").textContent = batch.error || [
      bridge.isPreview ? "预览模式 · 非硬件检测" : "候选尖峰 · 未做单元分离",
      batch.config.recordingPath ? `CSV ${batch.recordingComplete ? "已完成" : batch.running ? "记录中" : "待开始"}：${batch.recordedEvents} 条 · ${batch.config.recordingPath}` : "连续记录未启用",
      `界面缓存 ${history.events.length} 条`,
      history.missed ? `界面漏收 ${history.missed} 条` : "",
      history.evicted ? `缓存已淘汰 ${history.evicted} 条` : "",
      batch.discardedIncomplete ? `不完整片段已丢弃 ${batch.discardedIncomplete}` : "",
      batch.discontinuities ? `数据间断 / 伪迹段 ${batch.discontinuities}` : ""
    ].filter(Boolean).join(" · ");
    $("spike-record-status").classList.toggle("is-error", Boolean(batch.error || history.missed));
    if (!$("spike-view").classList.contains("active")) return;
    const mode = $("spike-display").value;
    $("spike-raster").hidden = mode !== "raster";
    $("spike-waveforms").hidden = mode !== "waveforms";
    $("spike-values").hidden = mode !== "values";
    $("spike-empty").hidden = mode !== "raster" || raster.points.length > 0;
    $("spike-empty-status").textContent = batch.config.enabled ? "等待候选尖峰" : "检测未启用";
    if (mode === "raster") {
      const container = $("spike-raster"); container.replaceChildren();
      container.style.setProperty("--raster-channels", String(channelCount));
      for (const channel of visibleChannels) {
        const row = document.createElement("div"); row.className = "spike-row";
        const name = document.createElement("span"); name.className = "spike-channel"; name.textContent = label(channel);
        const track = document.createElement("div"); track.className = "spike-track";
        for (const point of raster.points.filter((point) => point.channelIndex === channel)) {
          const marker = document.createElement("i"); marker.className = "spike-marker";
          marker.style.left = `${point.position * 100}%`; marker.title = `sample ${point.sampleIndex}`; track.appendChild(marker);
        }
        const rate = document.createElement("span"); rate.className = "spike-rate"; rate.textContent = `${raster.ratesHz[channel].toFixed(2)} Hz`;
        row.append(name, track, rate); container.appendChild(row);
      }
    } else if (mode === "waveforms") {
      const container = $("spike-waveforms"); container.replaceChildren();
      container.classList.toggle("single-channel", selectedChannel !== null);
      const plots = [];
      for (const channel of visibleChannels) {
        const events = history.snippets(channel);
        const tile = document.createElement("section"); tile.className = "spike-waveform-tile";
        const heading = document.createElement("header");
        const name = document.createElement("strong"); name.textContent = label(channel);
        const detection = thresholds[channel];
        const thresholdState = { calibrating: "校准中", flat: "噪声不足", saturated: "饱和", insufficient: "样本不足" }[detection?.state] || "—";
        const metric = document.createElement("span"); metric.textContent = `${counts[channel] || 0} 次 · 阈值 ${Number.isFinite(detection?.thresholdUv) ? detection.thresholdUv.toFixed(1) + " µV" : thresholdState}`;
        metric.title = Number.isFinite(detection?.noiseUv) ? `噪声 σ ${detection.noiseUv.toFixed(2)} µV · 基线 ${detection.baselineUv.toFixed(2)} µV` : "固定阈值或尚无噪声估计";
        if (Number.isFinite(detection?.noiseUv)) metric.textContent += ` · σ ${detection.noiseUv.toFixed(2)} µV`;
        heading.append(name, metric);
        const canvas = document.createElement("canvas"); canvas.setAttribute("aria-label", `${label(channel)} 尖峰波形`);
        tile.append(heading, canvas); container.appendChild(tile);
        plots.push({ canvas, events, detection });
      }
      for (const { canvas, events, detection } of plots) drawSpikeWaveforms(canvas, events, Number($("spike-scale").value), detection);
    } else {
      const body = $("spike-values-body"); body.replaceChildren();
      for (const event of history.events.filter((event) => selectedChannel === null || event.channelIndex === selectedChannel).slice(-200).reverse()) {
        const row = document.createElement("tr");
        for (const value of [label(event.channelIndex), `${event.stream}:${event.chipChannel}`, event.sampleIndex, (event.timeMs / 1000).toFixed(6), event.peakUv.toFixed(2), event.peakToPeakUv.toFixed(2), Number.isFinite(event.thresholdUv) ? event.thresholdUv.toFixed(2) : "—", Number.isFinite(event.noiseUv) ? event.noiseUv.toFixed(2) : "—"]) {
          const cell = document.createElement("td"); cell.textContent = String(value); row.appendChild(cell);
        }
        body.appendChild(row);
      }
    }
  }
  return { history, bind, poll, render };
}
