const COLORS = [
  "#42d3b0", "#69b9ff", "#f3b64b", "#ff7d6e",
  "#9b8cff", "#55c7d9", "#a8d35f", "#f38ab7"
];

export class WaveformRenderer {
  constructor(canvas, labels) {
    this.canvas = canvas;
    this.context = canvas.getContext("2d", { alpha: false });
    this.labels = labels;
    this.frame = null;
    this.frames = [];
    this.timeWindowMs = 250;
    this.gain = 1;
    this.paused = false;
    this.channelNames = [];
    this.visibleChannels = Array.from({ length: 16 }, () => true);
    this.pixelRatio = Math.min(window.devicePixelRatio || 1, 2);
    this.resizeObserver = new ResizeObserver(() => this.resize());
    this.resizeObserver.observe(canvas.parentElement);
    this.buildLabels(16);
    this.resize();
  }

  buildLabels(count) {
    this.labels.replaceChildren();
    for (let channel = 0; channel < count; channel += 1) {
      const button = document.createElement("button");
      button.className = "channel-label active";
      button.dataset.channel = String(channel);
      const channelName = this.channelNames[channel] || String(channel + 1).padStart(2, "0");
      button.title = `显示或隐藏通道 ${channelName}`;
      button.innerHTML = `<span style="--channel-color:${COLORS[channel % COLORS.length]}"></span><strong>${channelName}</strong>`;
      button.addEventListener("click", () => {
        this.visibleChannels[channel] = !this.visibleChannels[channel];
        button.classList.toggle("active", this.visibleChannels[channel]);
        this.draw();
      });
      this.labels.appendChild(button);
    }
  }

  setGain(value) {
    this.gain = Number(value) || 1;
    this.draw();
  }

  setTimeWindow(value) {
    this.timeWindowMs = Math.min(1000, Math.max(100, Number(value) || 250));
    this.draw();
  }

  setPaused(paused) {
    this.paused = paused;
  }

  setChannelNames(names) {
    this.channelNames = Array.isArray(names) ? names.slice() : [];
    this.buildLabels(this.visibleChannels.length);
  }

  setFrame(frame) {
    if (!frame || this.paused) return;
    if (this.frame?.sequence === frame.sequence) return;
    this.frame = frame;
    this.frames.push(frame);

    const maximumSamples = frame.sampleRateHz * 1.2;
    let retainedSamples = 0;
    let firstRetained = this.frames.length - 1;
    for (let index = this.frames.length - 1; index >= 0; index -= 1) {
      const candidate = this.frames[index];
      retainedSamples += candidate.pointCount * candidate.binWidthSamples;
      firstRetained = index;
      if (retainedSamples >= maximumSamples) break;
    }
    if (firstRetained > 0) this.frames.splice(0, firstRetained);

    if (this.visibleChannels.length !== frame.channelCount) {
      this.visibleChannels = Array.from({ length: frame.channelCount }, () => true);
      this.buildLabels(frame.channelCount);
    }
    this.draw();
  }

  resize() {
    const rect = this.canvas.parentElement.getBoundingClientRect();
    const width = Math.max(320, Math.floor(rect.width));
    const height = Math.max(240, Math.floor(rect.height));
    this.canvas.width = Math.floor(width * this.pixelRatio);
    this.canvas.height = Math.floor(height * this.pixelRatio);
    this.canvas.style.width = `${width}px`;
    this.canvas.style.height = `${height}px`;
    this.context.setTransform(this.pixelRatio, 0, 0, this.pixelRatio, 0, 0);
    this.draw();
  }

  draw() {
    const context = this.context;
    const width = this.canvas.width / this.pixelRatio;
    const height = this.canvas.height / this.pixelRatio;
    context.fillStyle = "#111719";
    context.fillRect(0, 0, width, height);
    this.drawGrid(context, width, height);
    if (!this.frame) return;

    const channelCount = this.frame.channelCount;
    const targetSamples = this.frame.sampleRateHz * this.timeWindowMs / 1000;
    const displayFrames = [];
    let selectedSamples = 0;
    for (let index = this.frames.length - 1; index >= 0; index -= 1) {
      const candidate = this.frames[index];
      displayFrames.unshift(candidate);
      selectedSamples += candidate.pointCount * candidate.binWidthSamples;
      if (selectedSamples >= targetSamples) break;
    }

    const totalPoints = displayFrames.reduce((sum, candidate) => sum + candidate.pointCount, 0);
    const laneHeight = height / channelCount;
    const xStep = width / Math.max(1, totalPoints - 1);
    for (let channel = 0; channel < channelCount; channel += 1) {
      if (!this.visibleChannels[channel]) continue;
      const centerY = laneHeight * (channel + 0.5);
      const scale = (laneHeight * 0.38 * this.gain) / 2048;
      context.strokeStyle = COLORS[channel % COLORS.length];
      context.globalAlpha = 0.86;
      context.lineWidth = 1;
      context.beginPath();
      let globalPoint = 0;
      for (const displayFrame of displayFrames) {
        for (let point = 0; point < displayFrame.pointCount; point += 1) {
          const index = point * channelCount + channel;
          const x = globalPoint * xStep;
          const minY = centerY - (displayFrame.minimums[index] - 32768) * scale;
          const maxY = centerY - (displayFrame.maximums[index] - 32768) * scale;
          context.moveTo(x, Math.max(centerY - laneHeight * 0.45, Math.min(centerY + laneHeight * 0.45, minY)));
          context.lineTo(x, Math.max(centerY - laneHeight * 0.45, Math.min(centerY + laneHeight * 0.45, maxY)));
          globalPoint += 1;
        }
      }
      context.stroke();
    }
    context.globalAlpha = 1;
  }

  drawGrid(context, width, height) {
    context.lineWidth = 1;
    context.strokeStyle = "rgba(213, 228, 230, 0.075)";
    context.beginPath();
    for (let x = 0; x <= width; x += 50) {
      context.moveTo(x + 0.5, 0);
      context.lineTo(x + 0.5, height);
    }
    const laneCount = Math.max(1, this.frame?.channelCount || this.visibleChannels.length || 16);
    for (let y = 0; y <= height; y += height / laneCount) {
      context.moveTo(0, y + 0.5);
      context.lineTo(width, y + 0.5);
    }
    context.stroke();
    context.strokeStyle = "rgba(213, 228, 230, 0.13)";
    context.beginPath();
    for (let x = 0; x <= width; x += 250) {
      context.moveTo(x + 0.5, 0);
      context.lineTo(x + 0.5, height);
    }
    context.stroke();
  }
}
