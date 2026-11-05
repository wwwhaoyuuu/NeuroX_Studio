# RHS Rust Control 验证记录

## 2026-10-30 - Full recording analysis export, offline only

- Added a full-recording export command and desktop dialog, separate from session
  summaries. Exports stream raw samples at full rate, physical channel addresses,
  original sample indices, TTL availability, block flags and recording parameters.
  Optional CSV contains microvolts. Original recordings are opened read-only.
- Native release tests: 63 passed, including three new export tests covering 16/64
  channels, legacy metadata, time gaps, missing TTL, CRC corruption, incomplete
  sources and refusal to overwrite existing destinations. Eleven frontend test
  groups passed. Browser checks covered dialog layout, source/destination linkage
  and preview refusal to fabricate a real file export.
- Three Python reader tests passed, including reading an actual native-exported
  64-channel fixture, ADC/microvolt conversion, sample indices and incomplete-file
  rejection. MATLAB R2019a could not start in either of two headless attempts;
  the MATLAB helper is provided but runtime verification is NOT claimed.
- No acquisition, impedance measurement or stimulation command was sent. Low disk
  space blocked the initial debug build; release-cache builds completed. Existing
  recordings were neither renamed nor rewritten.

## 2026-09-30 - MEA 64 stimulation routing, offline only

- The frontend requires exactly four distinct 16-channel RHS2116 streams: two on port B and two on port C. A separately verified B/C 16-channel group-to-stream order is required to build the 64-electrode route map.
- Synthetic tests cover all 64 unique physical addresses, selected-electrode resolution, wrong-port routes, duplicate addresses, missing streams and invalid group assignments. `npm test` and `npm run build` passed.
- The stream order for the connected 64-channel assembly has not yet been supplied in this validation record. The native acquisition and stimulation adapters still target one RHS2116 stream. No 64-channel hardware pulse or measured output is claimed; the desktop remains interlocked.

## 2026-09-30 - Explicit single-stream targeting on four-chip scans

- The native direct-stimulation session can now open a specific detected RHS2116 by port and command stream instead of implicitly choosing the first chip. Auxiliary magnitude commands are limited to that target stream. The guarded CLI requires explicit serial, port, stream and chip channel and accepts either one RHS2116 or a B/C two-plus-two four-chip scan.
- The CLI still stimulates only one physical channel per finite command. This is addressability groundwork, not simultaneous multichannel output or a 64-channel acquisition implementation. All workspace Rust tests and frontend route tests passed; physical output and the connected board's stream order remain unverified.

## 2026-08-18 - Iteration 1

### 验证范围

- Rust 核心数据模型；
- RHS2116 持久连续采集会话；
- 单硬件线程和有界原始数据队列；
- 32 位硬件时间戳回绕与断裂检测；
- SyntheticSource 和通用有界队列；
- CLI 真实采集完整性诊断；
- FrontPanel 打开失败的资源清理。

所有真实采集命令均保持 stimulation disabled，本轮没有发送刺激命令。

### 静态和自动化检查

```text
cargo fmt --all -- --check
PASS

cargo test --workspace
PASS: 12 passed, 0 failed

cargo clippy --workspace --all-targets -- -D warnings
PASS: 0 warnings
```

测试分布：

- rhs-core：4；
- rhs-device：3；
- rhs-pipeline：5。

### 模拟数据通路

命令：

```text
scripts\dev-cmd.bat synthetic-check --blocks 1000
```

结果：

```text
synthetic_blocks: 1000
queue_max_depth: 1
queue_rejected: 0
PASS
```

### 设备扫描

```text
FrontPanel serial: [redacted]
Device: Opal Kelly XEM7310
Chip: RHS2116
Port: D
Command stream: 6
Channels: 16
Pipe read error: 0
```

### 5 秒真实采集

```text
elapsed_seconds: 5.023
sample_blocks: 147
samples_per_channel: 150528
observed_samples_per_second: 29969.7
bytes_read: 20471808
timestamp_gaps: 0
queue_overflows: 0
PASS
```

### 1 分钟真实采集

```text
elapsed_seconds: 60.010
sample_blocks: 1758
samples_per_channel: 1800192
observed_samples_per_second: 29998.0
bytes_read: 244826112
timestamp_gaps: 0
queue_overflows: 0
PASS
```

### 10 分钟真实采集

```text
elapsed_seconds: 600.024
sample_blocks: 17579
samples_per_channel: 18000896
observed_samples_per_second: 30000.3
bytes_read: 2448121856
timestamp_gaps: 0
queue_overflows: 0
PASS
```

采集期间多次检查 `rhs-cli` 进程：

```text
Working set: approximately 8.3-8.4 MB
Private memory: approximately 7.5-7.7 MB
```

未观察到内存随采集数据量线性增长。停止后进程正常退出，随后能够重新打开设备并再次完成采集。

### 失败路径

使用不存在的设备序列号进行启动测试时，首次发现 Intan 旧版 `RHXController::open` 删除 `dev` 后未将指针置空，析构阶段会再次删除同一指针。

修复本地 vendored Intan API 后重新测试：

```text
ERROR: OpenFailed: failed to open FrontPanel device
exit code: 1
```

进程约 4.4 秒内正常结束，没有挂起。

### 尚未验证

- 运行中物理拔出 USB 后的停止延迟和 Fault 事件；
- 电极接入后的信号质量、噪声、饱和和单位换算；
- 原始文件记录与回放；
- 采集期间同时刺激；
- 示波器或测试负载上的真实刺激波形。

输入目前未连接电极，原始样本出现接近全量程的值不能用于判断生理信号质量。

## 2026-08-18 - Iteration 2

### 验证范围

- Source -> Processor -> Sink 生命周期、取消和错误传播；
- 带 CRC32、完成标志和尾部计数的版本化原始文件；
- 设备序列号、端口、stream、通道映射和 bitfile SHA-256 元数据；
- 模拟数据确定性记录/回放；
- 真实 RHS2116 只读记录和独立文件回放；
- 截断、内容损坏、中途失败和防覆盖路径。

本轮硬件记录始终为 `stimulation=disabled`。

### 自动化检查

```text
cargo fmt --all -- --check
PASS

cargo test --workspace
PASS: 22 passed, 0 failed

cargo clippy --workspace --all-targets -- -D warnings
PASS: 0 warnings
```

测试分布：rhs-core 4、rhs-device 3、rhs-pipeline 7、rhs-storage 8。存储测试覆盖逐块完全回放、元数据往返、SHA-256 标准向量、CRC 损坏、尾部截断、序号断裂和未完成文件恢复。

### 5 秒真实记录

```text
FrontPanel serial: [redacted]
Port / stream: D / 6
Channels: 16
Sample rate: 30000 Hz
Elapsed: 5.019 s
Blocks: 146
Samples per channel: 149504
File bytes: 5089891
Timestamp gaps: 0
Queue overflows: 0
Bitfile SHA-256: a93911b1d49d7c51e922cbbaa9aa0241690e845e5b3e3682ef833db95b5016f3
PASS
```

记录文件：`work\iteration2-rhs-20260818-01.rhsraw`。

### 独立回放

```text
Format version: 1
Complete: true
Source: rhs-port-D
Blocks: 146
Samples per channel: 149504
Discontinuities: 0
CRC/footer/sequence/sample index: PASS
Bitfile fingerprint: match
```

PowerShell `Get-FileHash` 得到相同 bitfile SHA-256，独立验证了 Rust 实现。对同一输出路径再次执行 `record` 返回文件已存在，记录文件 SHA-256 在操作前后均为 `121b3d0eaf89d2f589474532330c1708b9f495bd31784caf47d1b7be0097a213`；随后设备扫描仍成功。

### 已知边界

- 当前只读管线没有产生运行时业务事件；Event 总线加入后需补事件序列化和回放；
- 自动化测试证明 I/O/格式错误会向上传播且文件保持 incomplete，真实磁盘写满仍待人工故障测试；
- 原始值范围包含未接电极时的浮空/饱和值，不代表生理信号质量或电压换算正确；
- 运行中物理拔出 USB 的 Fault 行为仍待人工验证。

## 2026-08-18 - Iteration 3

### 验证范围

- 包络降采样 Processor；
- 状态跨块保持的 50 Hz notch 与 300-3000 Hz band-pass；
- TTL 上升沿触发、可跨块延续的刺激伪迹 blanking；
- 带绝对采样点和 refractory 的阈值事件检测；
- 固定容量、丢最旧帧的 Viewer Sink；
- 每节点处理耗时及 Viewer 排队延迟指标；
- 离线参考、在线分块一致性和真实硬件处理负载。

本轮所有真实硬件命令均为 `stimulation=disabled`。

### 自动化检查

```text
cargo fmt --all -- --check
PASS

cargo test --workspace
PASS: 35 passed, 0 failed

cargo clippy --workspace --all-targets -- -D warnings
PASS: 0 warnings
```

`rhs-nodes` 的 13 项测试覆盖：包络和 TTL 合并、blanking 跨块状态、阈值采样点、事件队列满时显式失败、Viewer 丢旧策略、节点指标、滤波 Nyquist 校验以及频域结果。

频域验收条件：

```text
50 Hz notch: 50 Hz amplitude < 10% of input
50 Hz notch: 500 Hz amplitude > 90% of input
300-3000 Hz band-pass: 50 Hz amplitude < 10% of input
300-3000 Hz band-pass: 1000 Hz amplitude > 80% of input
300-3000 Hz band-pass: 8000 Hz amplitude < 15% of input
```

同一确定性信号按整段离线处理与 1024 点分块在线处理，输出 `u16` 样本逐个完全相等。

### 5 秒真实处理和慢 Viewer 测试

```text
FrontPanel serial: [redacted]
Port / stream: D / 6
Channels: 16
Sample rate: 30000 Hz
Elapsed: 5.022 s
Processed blocks: 146
Samples per channel: 149504
Timestamp gaps: 0
Acquisition queue overflows: 0
Viewer frames enqueued: 146
Viewer frames dropped oldest: 142
Viewer max depth: 4
Latest viewer points: 256
Threshold events: 0
PASS
```

本测试故意在采集期间不读取 Viewer 队列。Viewer 只保留最后 4 帧，最新帧序号与最后处理块一致；采集仍无断裂或溢出。

真实节点耗时：

```text
continuity       average 0.9 us,   max 8.1 us
artifact blanker average 57.6 us,  max 84.7 us
50 Hz notch      average 940.7 us, max 1101.7 us
band-pass        average 943.5 us, max 1197.3 us
threshold        average 21.0 us,  max 59.8 us
viewer envelope  average 490.9 us, max 729.6 us
```

每块包含 1024 点，约对应 34.1 ms 数据时间；当前串行节点总平均处理耗时约 2.45 ms/块，保留了明显实时余量。

### 已知边界

- 滤波输出仍是以 32768 为中点重新量化的 `u16`，尚未完成真实电压单位标定；
- blanking 当前由 TTL 上升沿驱动，尚未接入未来 StimResult 事件；
- 阈值检测已经产生有界事件，但事件尚未写入原始记录文件；
- Viewer 当前只有后端帧队列，还没有 Tauri 波形界面；
- 未接电极时的输入不能用于评价滤波后的生理信号质量。

## 2026-08-18 - Iteration 4（安全服务与可取消硬件会话阶段）

### 验证范围

- 明确配置、无默认生物参数的刺激安全策略；
- 预设白名单和 Arm 会话；
- 命令 ID、执行窗口、截止时间和最小间隔；
- 每命令和每 Arm 会话脉冲数限制；
- 单相电荷、占空比、脉冲串时长及 FPGA 时序上限；
- 排队/活动命令取消、Emergency Stop 和设备 Fault 自动 Disarm；
- StimResult 与 Event 审计；
- C ABI `open/start/poll/cancel/close` 持久刺激会话；
- Rust `StimHardwareSession` 与真实 `StimExecutor`；
- 固定台架预设和两个独立物理确认门禁；
- 默认关闭旧同步真实刺激入口。

本阶段没有发送任何真实刺激命令。

### 自动化检查

```text
cargo test --workspace
PASS: 54 passed, 0 failed

cargo clippy --workspace --all-targets -- -D warnings
PASS: 0 warnings
```

`rhs-stim` 的 12 项测试覆盖 Disarmed 拒绝、正常执行、重复命令、累计脉冲限制、截止时间、排队/活动取消、Emergency Stop、Fault、Arm 到期、最小命令间隔、预设范围和单相电荷上限。

### CLI 安全服务演练

命令：`scripts\dev-cmd.bat stim-service-check`

```text
mode: dry-run (no hardware output)
arm_session: 1
phase_charge_limit_check_pc: 2
executor_starts: 2
executor_cancels: 1
audit_events: 9
audit_results: 7
final_state: Disarmed
PASS
```

演练顺序为 Arm、排队并完成、排队后取消、活动时 Emergency Stop、停止后再次提交并被 NotArmed 拒绝。

### 无输出硬件会话回归

命令：`scripts\dev-cmd.bat stim-session-check`

```text
mode: hardware session safety check (no stimulation start)
ready: stimulation session ready with output disabled on RHS2116 port D, stream=6
cancel: idle stimulation session forced to output-disabled state
pipe_read_error: 0
active: false
PASS
```

此命令只执行 open、空闲 cancel 和 close，不调用 start。关闭会话后再次只读扫描，仍检测到已连接设备（序列号已脱敏）、RHS2116、Port D、stream 6，pipe error 0，证明资源已释放且设备可重新打开。

本轮使用的默认 bitfile SHA-256 为 `A93911B1D49D7C51E922CBBAA9AA0241690E845E5B3E3682EF833DB95B5016F3`。

### 真实入口门禁与旧入口隔离

只提供 `--confirm-dummy-load`、缺少 `--confirm-measurement-ready` 时，CLI 在打开 FrontPanel 前返回：

```text
ERROR: stim requires both --confirm-dummy-load and --confirm-measurement-ready
```

真实入口只接受固定 `bench-single-v1` 或 `bench-train-v1` 预设，并要求操作员名称。默认 Rust 构建不暴露 `RhsDevice::run_bounded_stim`；历史同步实现仅在显式启用 `legacy-direct-stim` feature 时编译，兼容编译检查通过。

### 尚未完成

- 真实硬件 Emergency Stop 的停止延迟测量；
- 老师批准的正式参数策略和预设；
- 测试负载上的单脉冲及有限脉冲串；
- 示波器/电流测量得到的幅值、极性、相宽、间隔和次数；
- 测量参数、软件参数与波形截图的关联保存。

因此 Iteration 4 的软件实现已闭环，但物理验收仍处于进行中，不能把 dry-run、空闲 cancel 或 API 成功视作实际刺激波形正确。

## 2026-08-18 - Iteration 5（同步采集与刺激软件通路）

### 验证范围

- acquisition 与 stimulation 共用一个 C++/FrontPanel 硬件会话和一个 Rust 硬件线程；
- 开始采集前预装有限刺激配置，只有到期的调度命令才短暂启用手动触发；
- 有界命令队列、截止采样点、重复 ID 和队列溢出拒绝；
- 每采样点返回 FPGA `stim_on`、极性、compliance 和 DC amplifier 元数据；
- `Queued -> Triggered -> Observed -> Completed` 事件及 cancel/Fault 路径；
- `rhs-stim` 安全服务到采样时钟调度器的 `rhs-runtime` 适配；
- 预装刺激配置但不发送 trigger 的真实板卡回归。

本阶段没有发送任何真实刺激命令。

### 自动化检查

```text
cargo test --workspace
PASS: 62 passed, 0 failed

cargo clippy --workspace --all-targets -- -D warnings
PASS: 0 warnings
```

新增测试覆盖有界排序队列、过期/重复/溢出拒绝、基于 FPGA 标记的有限刺激完成采样点、空载 compliance 不误停采集、安全参数与预装配置一致性、采样时钟调度和 cancel 确认。

### 无触发统一会话回归

命令：`scripts\dev-cmd.bat sync-session-check --seconds 2`

```text
device_serial: [redacted]
port / stream: D / 6
sample_rate_hz: 30000
preset_preloaded: true
stim_cmd_mode: disabled until a scheduled command is due
blocks: 59
samples_per_channel: 60416
next_sample_index: 60416
stim_flagged_blocks: 0
stim_events: 0
timestamp_gaps: 0
queue_overflows: 0
PASS
```

同一版桥接代码随后完成 2 秒真实记录、独立回放和 Viewer 回归：记录文件已完成并通过 CRC、footer、序号和 bitfile hash 检查；采集时间戳断裂和队列溢出均为 0。

### 尚未完成

- 采集期间发送真实 trigger 并观察 FPGA `stim_on`；
- 软件触发延迟、抖动和实际电流波形测量；
- 刺激伪迹、输入饱和与恢复时间；
- 长时间有限刺激压力测试；
- 将刺激审计事件持久化到记录文件；
- 根据实测延迟决定是否把闭环调度下沉到 FPGA。

因此本轮只证明统一软件通路、队列和无触发采集回归成立，不证明电极或小鼠端已经产生刺激。

## 2026-08-18 - Iteration 6（Tauri 桌面界面首版）

### 验证范围

- Tauri 2 桌面壳和单一 Rust 会话后端；
- 设备扫描、开始/停止采集、开始/停止 RHSRAW 记录和管线配置命令；
- 独立的状态/事件 IPC 与最多 360 点/帧的 min/max 波形 IPC；
- 100 ms、250 ms、500 ms 和 1 s 有界波形历史；
- 16 通道开关、增益、暂停、指标、事件筛选和性能页面；
- Stim Pilot 可见但 Arm/Trigger 硬锁定，Emergency Stop 独立可用；
- 浏览器演示模式不加载 FrontPanel，也不调用任何真实硬件命令。

本阶段没有发送硬件刺激命令，也没有执行新的真实板卡采集。

### 自动化检查

```text
npm run build
PASS

npm test
PASS: frontend assets, critical controls and stimulation interlock are present

cargo test --workspace
PASS: 65 passed, 0 failed

cargo clippy --workspace --all-targets -- -D warnings
PASS: 0 warnings

cargo build -p rhs-desktop
PASS
```

### 视觉和交互检查

- 1280 x 720：固定工具栏、三列工作区、16 通道波形、指标条和事件台无重叠或横向滚动；
- 840 x 900：工作区切换为单列，左右配置面板顺序堆叠，无元素越出页面；
- Start/Stop、记录开关、处理节点开关、急停和事件记录在演示模式中可交互；
- 1 s 时间窗可切换并持续绘制，波形历史按采样数限制在约 1.2 秒；
- Arm、Trigger 和刺激预设保持禁用；静态假磁盘占用条已移除。

### 尚未完成

- 原生 Tauri 窗口重载、关闭、后端崩溃和恢复测试；
- 真实设备拔插、占用和 USB 故障测试；
- 磁盘剩余容量、慢盘、磁盘满和权限错误显示；
- 长时间波形显示、记录和 UI 压力测试；
- 完成物理台架验收后的 Arm、Trigger 和 Disarm。

因此 Iteration 6 已具备可运行的首版桌面工作台，但故障恢复、磁盘监控和真实刺激控制仍属于后续验收项。

## 2026-08-19 - Iteration 7（可配置内置管线）

### 验证范围

- `PipelineConfig v1` 保存有序节点、稳定 ID、启用状态和全部参数；
- Rust 节点注册表声明 `SampleBlock` 端口、参数范围、实例上限以及采样率/通道保持能力；
- 按配置顺序构建 artifact blanking、notch、band-pass 和 threshold，多 threshold 事件接收器可并存；
- 版本、重复 ID、30 kHz Nyquist、16 通道、TTL、消隐长度和不应期在替换链路前检查；
- 前端支持添加、删除、启停、上下移动、展开参数以及本地方案保存/载入；
- 旧固定开关配置可迁移到 v1，未知版本或节点会明确拒绝；
- 原始记录在处理链前写入，刺激服务没有注册为普通节点。

本轮仅使用浏览器演示数据进行交互检查，没有扫描板卡、开始新的真实采集或发送刺激命令。

### 自动化检查

```text
cargo check -p rhs-desktop --config .\work\cargo-direct.toml
PASS

cargo test -p rhs-desktop --config .\work\cargo-direct.toml
PASS: 7 passed, 0 failed

cargo test --workspace --config .\work\cargo-direct.toml
PASS: 69 passed, 0 failed

cargo clippy --workspace --all-targets --config .\work\cargo-direct.toml -- -D warnings
PASS: 0 warnings

node --check apps\rhs-desktop\src\app.js
node --check apps\rhs-desktop\src\bridge.js
PASS

node apps\rhs-desktop\scripts\build.cjs
node apps\rhs-desktop\scripts\test-ui.cjs
PASS
```

Rust 测试覆盖节点顺序、多 threshold 接收器、重复 ID、未知版本、通道越界、Nyquist 不兼容和前后端 JSON 字段往返。

### 视觉和交互检查

- 1280 x 720 三列工作区无重叠，处理链在独立滚动侧栏内，不压缩波形区；
- 实际完成添加节点、参数修改、上移节点，后端状态均显示按当前顺序重建；
- 把带通高截止设为低于低截止时配置被拒绝，修复为合法值后恢复；
- 方案保存后可从列表载入，并把临时增加的 6 节点管线恢复为保存的 5 节点管线；
- Arm、Trigger 和刺激预设仍保持禁用。

### 尚未完成

- 新配置编辑器在原生 Tauri WebView 中的长时间压力和崩溃恢复；
- 运行中频繁重建复杂管线时的真实硬件负载测试；
- 文件导入/导出和跨电脑方案迁移；当前方案保存在本机应用存储；
- 动态第三方插件；当前评估为不需要，未来如引入必须增加进程隔离和单独安全评审。

## 2026-09-04 - Unicode bitfile and current hardware path

- Windows FrontPanel driver enumerated an Opal Kelly XEM7310 (serial redacted, firmware 1.30).
- Replaced the filename-based FPGA upload with Unicode-safe file reading followed by
  `ConfigureFPGAFromMemory`; no FPGA or Verilog content was changed.
- The 2024 RHS bitfile loaded successfully and reported Rhythm version 1.
- Scan detected one RHS2116 on port A, command stream 0, with 16 channels.
- A two-second read-only acquisition produced 59 blocks and 60,416 samples per channel at
  29,925.6 samples/s, with zero timestamp gaps and zero queue overflows.
- A subsequent three-second signal-quality capture again had zero timestamp gaps and zero
  queue overflows. All 16 channels touched at least one 16-bit ADC rail and all 16 had a
  correlation of at least 0.95 with the channel common mode. This confirms that the
  periodic display waveform is present in the raw samples and is consistent with an
  unterminated/reference-deficient analog input, not a WebView rendering artifact.
- Stimulation remained disabled for the entire scan and acquisition check.

## 2026-09-30 - RHS2116 stimulation readiness check

- The desktop maps `CH-01` through `CH-16` exactly to physical chip channels 0 through 15;
  malformed and out-of-range labels are rejected before software Arm.
- Desktop tests: 20 passed. Device, runtime and stimulation safety tests: 11, 3 and 12 passed.
- `stim-session-check` found one RHS2116 on port A, stream 0. Session open, idle cancel and
  close passed with output disabled and USB pipe error 0.
- `sync-session-check` preloaded the fixed bench preset and acquired 60,416 samples per
  channel in 59 blocks. It reported zero timestamp gaps, queue overflows, stimulation
  events and FPGA `stim_on` markers. No trigger command was sent.
- The desktop acquisition request still sets `stimulation: None`, and its Arm/Trigger
  workflow remains interlocked. The native bridge currently programs one RHS2116
  channel per preset; selecting multiple channels in the desktop does not program
  multiple physical outputs.
- The test-load pulse shape, current, phase timing and active Emergency Stop latency
  remain unmeasured. This check does not qualify real stimulation for distribution.
- The guarded CLI bench entry now accepts `--channel 1..16` and maps it to chip channels
  0..15. All 16 addresses and both out-of-range boundaries passed unit tests. An actual
  invocation using channel 16 without the two physical confirmations was rejected before
  opening the device. No per-channel output waveform has been measured.

## 2026-09-30 - 16-channel preset preload, no trigger

- Added a 16-bit RHS2116 channel mask to the native acquisition stimulation configuration.
  The direct bounded stimulation session remains restricted to one channel.
- Desktop acquisition can preload the selected RHS2116 channels when the request matches
  the fixed 10 nA biphasic bench pulse or train. Custom waveforms and magnitudes are rejected
  for hardware preload; desktop Arm/Trigger remain interlocked software operations.
- On the connected port A RHS2116, `sync-session-check --channel-mask 0xffff` configured all
  16 channels and acquired 59 blocks / 60,416 samples per channel without sending a trigger.
  There were zero `stim_on` markers, stimulation events, timestamp gaps, or queue overflows.
- After closing RHS Studio, the same 16-channel no-trigger check passed again on
  serial [redacted]. `stim-session-check` also passed: idle cancel left output disabled,
  USB pipe error was 0, and the session reported inactive before close.
- Desktop tests passed (22), along with device/runtime/CLI tests. Actual current, phase width,
  channel isolation, compliance and Emergency Stop timing under load remain unmeasured.
  The desktop's charge-recovery toggle is not evidence that active charge-recovery timing
  has been programmed or validated in the RHS2116; the fixed bench waveform is balanced
  biphasic, but active recovery requires separate implementation and measurement.

## 2026-10-09 - Intan channel numbering and stimulation sequence comparison

- A scan earlier in this session detected four RHS2116 chips on a local unit (serial redacted):
  port B command streams 2/3 and port C command streams 4/5, with 32 channels per port
  and USB pipe error 0. All four targeted session-open/idle-cancel/close checks passed
  with stimulation output disabled.
- Compared the supplied RHX `ControllerInterface::addAmplifierChannels` and
  `setStimSequenceParameters` against the vendored Intan scan/controller API.
  RHX assigns port-local amplifier numbers in ascending command-stream order.
  Thus B-000..015 uses stream 2, B-016..031 stream 3, C-000..015 stream 4,
  and C-016..031 stream 5 for this scan. Chip channels are port-channel modulo 16.
  The probe routing module can now derive this order without a separately supplied
  stream-group table; the workbook still defines electrode-site positions.
- Fixed a direct stimulation-session defect: configuration disabled its trigger but
  the start path never enabled it. Explicit start now clears the manual trigger and
  enables the selected channel; completion/cancel disables all 16 trigger sequencers
  on that selected chip.
- Centralized the three native timing-programming paths. Nonzero interphase delay now
  uses `BiphasicWithInterphaseDelay` and sets the second-phase-start event, matching RHX.
  Zero-gap waveforms use `Biphasic`; all twelve timing events are explicitly written.
- `scripts/test-native-stim.bat` passed using a simulated controller: 64 independent
  addresses, both waveform shapes, single/train event timing, explicit enable and stop.
  All 93 Rust workspace tests, rustfmt, Clippy with warnings denied, and the frontend
  stimulation routing test passed. These are software tests, not output measurements.
- Attempted to open the existing RHX executable. Its first launch exposed a
  Configuration File Error, subsequent launches exposed no targetable RHX window,
  and Task Manager capture timed out. Process termination from the shell was denied.
  The live RHX device and stimulation screens were therefore not inspected this turn.
- No physical stimulation trigger was sent. The desktop acquisition still selects one
  16-channel chip; the automatic 64-channel routing module is not yet integrated with
  a four-chip acquisition/stimulation desktop workflow. Real pulse current/timing and
  simultaneous four-chip acquisition remain unverified.

## 2026-10-09 - Unified 64-channel desktop bench implementation

- Supersedes the single-stream desktop limitation noted above. Acquisition now
  supports either one 16-channel chip or exactly streams 2/3/4/5 for 64 channels.
  Physical channel IDs, block sizing, amplifier/DC parsing and 64-bit stimulation
  and compliance feedback use the actual enabled streams.
- Hardware preset conversion maps every B/C address to its chip and channel.
  Native configuration programs selected channels on each chip; Trigger enables
  the selected sequencers and asserts their common manual trigger. Continuous
  monitor commands preserve programmed magnitudes and DAC step size.
- Added custom representable current and quantized timing for shared symmetric
  negative-first biphasic sequences. Triphasic and active charge recovery remain
  unsupported. The UI label now says balanced biphasic, not active recovery.
- Desktop hardware Arm requires exact preloaded parameters, verified chip layout,
  operator identity and explicit dummy-load bench confirmations. Arm expires in
  60 seconds. Cancel/Disarm/Emergency Stop latch hardware output off until restart.
- Bounded atomic queue submission, selected-channel compliance checks, all-selected
  channel stim-on completion, timestamp-gap shutdown and flushed JSONL audit were
  added. Browser preview cannot enable real output.
- 99 Rust workspace tests and all seven frontend test groups passed; Clippy passed
  with warnings denied. Native simulated-controller tests passed for all 64
  independent addresses, all-channel enable, selective enable and finite timing.
- Browser inspection confirmed 64 electrode buttons, the probe-site layout,
  desktop dialog bounds and disabled real-output control in preview. The standalone
  Edge screenshot harness could not start its DevTools endpoint in this environment;
  its automated screenshot checks were not reported as passing.
- No physical trigger was sent during this implementation. Simultaneous four-chip
  acquisition, load current, pulse timing, channel isolation, compliance behavior
  under load and active stop latency still need hardware measurements. This is a
  bench-validation release, not a qualified biological-use or full RHX-parity release.

## 2026-10-09 - Installed client and live no-trigger verification

- Replaced the local installed executable, bitfile and runtime DLLs with the bench
  build, after backing up the previous installation. Desktop and Start menu links
  now target the installed executable directly; read-back target paths and installed
  file hashes were verified.
- The installed native client loaded its embedded `http://tauri.localhost/` frontend
  without a port-1420 server. Accessibility text showed MEA 64, four waveform pages,
  IDLE, DISARMED and INTERLOCKED. The user confirmed the visible interface looked
  normal. Windows screenshot capture timed out and pointer input lacked geometry;
  automated native click/screenshot testing was not completed.
- Fixed an acquisition-start bug where selecting no stimulation channels still
  attempted to preload an invalid empty preset. Empty selection now starts acquisition
  without a stimulation preset; selected 16/64-channel presets still preload normally.
  Regression checks cover both profiles and unsupported 120-channel preload.
- Added read-only CLI `--channels 16|64` diagnostics and `sync64-session-check`.
  The same acquisition worker/native bridge as the desktop was tested on serial
  the same local unit (serial redacted), detecting exactly B2/B3/C4/C5 and USB pipe error 0.
- Five-second 64-channel acquisition read 146 blocks and 149,504 samples per channel,
  with observed rate 29,895.2 samples/second/channel, zero timestamp gaps and zero
  queue overflows. Open-input data showed strong approximately 50 Hz interference
  and rail saturation; this was not a biological signal quality validation.
- A real four-chip configuration check preloaded the fixed 10 nA balanced biphasic
  preset onto all 64 addresses without scheduling a trigger. Subsequent acquisition
  read 59 blocks / 60,416 samples per channel, with zero stim-artifact blocks,
  stimulation events, timestamp gaps and queue overflows.
- 100 Rust workspace tests and all frontend test groups passed after the correction.
  These checks validate acquisition and disabled-output preset programming, not
  delivered current, pulse timing, electrode isolation or loaded stop latency.

## 2026-10-22 - B/C selection on an eight-stream board

- A live scan of the later test unit (serial redacted) reported RHS2116 command streams 0 through 7 across
  ports A/B/C/D, with USB pipe error 0. Before the fix, read-only 64-channel
  acquisition failed with the strict B2/B3/C4/C5 layout error despite those four
  streams being present. This was a stream-selection limitation, not USB failure.
- Compared the supplied Intan controller's per-stream `enableDataStream` behavior
  and `addAmplifierChannels` indexing. Selected data are parsed using compact
  enabled-stream positions, while hardware commands and channel metadata preserve
  the physical command stream. No vendor driver or FPGA changes were needed.
- Native selection now extracts B2/B3/C4/C5 from a valid four-to-eight-stream scan.
  Extra A/D streams are disabled, all eight streams' stimulation triggers are
  disabled at acquisition initialization, and target port/stream metadata is set
  to selected B2 rather than the first chip found on A. Missing B/C streams,
  duplicate addresses and inconsistent port/stream pairs still fail closed.
- Frontend probe-route validation follows the same subset rule. Native regression
  cases cover four/eight-chip scans, shuffled order, missing streams, duplicates,
  wrong port/type, unsupported channel counts and simulated A/D trigger isolation.
  Frontend cases verify identical probe routing with four or eight detected chips.
- On that test unit after the fix, 64-channel read-only acquisition collected 147 blocks
  / 150,528 samples per channel in 5.027 seconds (29,943.1 samples/second/channel),
  with zero timestamp gaps and zero queue overflows. Target metadata was B/stream 2.
- All-64-channel fixed-preset preload then collected 59 blocks / 60,416 samples
  per channel using streams [2,3,4,5], with zero stimulation markers, stimulation
  events, timestamp gaps and queue overflows. The 16-channel no-trigger preload
  regression on A/stream 0 also collected 59 blocks with all four counters zero.
- 100 Rust tests, frontend test groups and native simulated-controller tests passed.
  No real stimulation trigger was sent. This validates selected-stream acquisition
  and disabled-output parameter programming, not delivered pulses or physical
  electrode isolation, and does not add full 128-channel support or full RHX parity.

## 2026-10-22 - Desktop B-000 feedback and unresolved worker disconnect

- Corrected manual trigger indices 0..7 to sequencer sources 24..31, matching the
  supplied Intan implementation. Offline native tests model the matching trigger
  edge, including rejection of digital source 0 for manual trigger 0.
- The user operated the installed RHS Studio, not a CLI trigger, on that test unit.
  Audits `stim-1792670358778689000`, `stim-1792670409295769700` and
  `stim-1792670433325754300` each contain one Queued / Triggered / Observed /
  Completed chain. Each selects only B-000 / stream 2 / channel 0 with stream mask
  [0,0,1,0,0,0,0,0], 10 nA, 200 us phases, 100 us gap and one biphasic pulse.
- Observed scheduling errors were 694, 674 and 709 samples (23.133, 22.467 and
  23.633 ms). Completion indices are derived from the observed marker and finite
  programmed timing; they are not external measurements of pulse width or current.
  All audit records explicitly state currentMeasured=false.
- Subsequent attempts, including fresh acquisition sessions, ended with
  `acquisition worker disconnected` without complete feedback. Output cannot be
  inferred for incomplete attempts. The UI entered FAULT / DISARMED / INTERLOCKED.
  Three successes do not qualify long-run stability or all 64 physical outputs.
- Found a deterministic diagnostic defect: the desktop consumed only one worker
  status on disconnect, allowing an old Started status to obscure the actual
  Fault. It now skips startup statuses, retains terminal errors through join,
  preserves panic detail and remaining stimulation events, and publishes final
  discontinuity/overflow counters rather than leaving the last healthy snapshot.
- Duplicate Arm is rejected before creating a replacement audit or resetting the
  lifetime/completion counter. Frontend controls reject concurrent stimulation
  actions and show a newly reported acquisition fault explicitly.
- 110 Rust tests, all seven frontend groups and Clippy with warnings denied passed.
  New offline tests reproduce Started-before-Fault, terminal-error propagation,
  panic detail, repeated Arm preservation and stimulation button states. No new
  hardware trigger was sent for these diagnostic changes. The underlying worker
  termination cause remains unresolved pending a fresh desktop diagnostic trace.

## 2026-10-22 - Bounded repeat and production-loop stop regressions

- Isolated the acquisition I/O boundary so the same production worker loop runs
  against an offline simulated transport in tests. The real driver, initialization,
  physical stream mapping and sample processing are unchanged by that boundary.
- New production-loop tests pass three sequential finite commands with continuous
  acquisition, emergency cancellation before any trigger, emergency cancellation
  of active and queued future commands, and injected trigger/read failures.
  Faulted commands emit Fault, never false Completed, and retain the original
  terminal error. Emergency-stop latching rejects later commands until restart.
- Added an explicitly selected no-bio repeated functional mode: one fixed pulse
  per manual Trigger, at most three submissions per Arm, at least one second
  between submissions, and the unchanged 60-second lifetime. Default single-shot
  mode remains one submission. No automatic repeat sequence is enabled here.
  The budget counts submitted commands, including attempts without completion.
- 115 Rust tests, all seven frontend test groups, native simulated-controller
  tests and Clippy with warnings denied passed. None of these tests uses a real
  electrical load, emits hardware pulses or measures emergency-stop latency.
- Subsequent single-shot user audit logs include complete sequences on the
  diagnostic build, but do not reproduce the old worker exit with a specific
  error. The original disconnect root cause is not claimed solved. Live GUI
  repeated-trigger and pending/active emergency-stop verification are pending.
- Known-load current, phase widths, biphasic waveform and cell-culture MEA
  experiment readiness remain unverified. At this checkpoint no biological-use
  mode was enabled; the separate configurable research mode is documented below.

## 2026-10-23 - Configurable research output

- Added a distinct hardware-configurable mode for existing 16/64-channel routes.
  It requires an operator and explicit routing, parameter-review and unverified
  output acknowledgements, without inventing dummy-load or instrument checks.
  Ambiguous confirmation modes, stale preload and mismatched layouts fail closed.
- Arm exposes quantized current, phase width, gap, frequency, pulse count and
  nominal charge per phase. Existing current/timing bounds remain unchanged;
  delayed biphasic is supported, triphasic and active recovery are not.
- Configurable output has a cumulative 16-command budget per 60-second Arm,
  checked arithmetic, remaining-lifetime checks and inter-submission spacing.
  Mode changes clear acknowledgements. Editing requires stopped acquisition;
  real output remains unavailable in browser preview.
- 119 Rust workspace tests and all seven frontend groups passed. New tests cover
  16/64 routing, explicit mode separation, quantization, cumulative budget,
  timing overflow, remaining lifetime and truthful audit confirmations. Native
  simulated-controller regressions also passed without hardware output.
- Browser preview at 1280x720 showed the three distinct acknowledgements and
  a fitting 16-channel editor with hardware bounds and triphasic disabled.
  Browser error logs were empty. Preview uses synthetic data, not a live board.
- No new real pulse was emitted for these changes. Physical current, waveform,
  all-channel isolation, live repeated-trigger/stop latency and cell-culture
  suitability remain unverified. The earlier worker-disconnect root cause is
  not claimed resolved by offline tests or successful single-shot feedback.

## 2026-10-23 - Third-trigger compliance fault diagnosis

- Read the user-operated installed-client audit `stim-1792687724177973100`.
  B-000 / command stream 2 / chip channel 0 used nominal 10 nA, 200 us phases,
  100 us gap and one pulse. Commands 1 and 2 each reported Queued, Triggered,
  Observed and Completed. Command 3 reported Queued and Triggered, followed by
  RHS2116 compliance warning at sample 364262 with compact mask 0x0001.
  The session ended with that explicit ProtocolError, not an unknown worker exit.
- Read-only inspection of the running client confirmed FAULT / DISARMED /
  INTERLOCKED and the same compliance error. Acquisition had stopped, leaving
  the last waveform frame visible. No additional trigger was sent for diagnosis.
  Compliance indicates reported insufficient current-source voltage headroom;
  the audit alone does not distinguish an open circuit, return path, electrode
  impedance, supply problem or prove delivered current. No physical measurements
  were made. This identifies this incident, not every earlier worker disconnect.
- Kept fail-closed output and acquisition shutdown. The worker now latches
  stimulation immediately on compliance detection and reports physical channel
  labels alongside the full 64-bit mask. Terminal desktop faults preserve the
  completed/submitted counts, last command, programmed settings and audit path,
  while clearing Arm and pending commands and forcing INTERLOCKED.
- Added a persistent fault band above the views and explicit fault messaging in
  the stimulation panel. Retained waveforms are labelled non-live. The canvas
  now respects available dimensions when the fault band reduces plot height.
- 122 Rust tests, all seven frontend groups and Clippy with warnings denied
  passed. A new production-loop simulated-I/O test completes two commands,
  injects compliance on the third, faults a queued fourth, rejects new commands,
  and asserts no false completion. Address tests include bit 63 and odd streams;
  desktop and frontend tests preserve counts and disable faulted controls.
- Synthetic UI checks at 1280x720, 1040x680 and 1460x920 showed the fault band;
  the final desktop layout keeps the canvas above its readout without overlap.
  Browser errors were empty. The fixture is excluded from packages and never
  connects to hardware. Neither output settings nor compliance protection was
  weakened; live loaded repeat testing remains outstanding.

## 2026-10-23 - Hardware impedance path

- Replaced desktop impedance preparation-only behavior with a background native
  measurement worker for 16/64-channel profiles. Requests validate explicit AC
  injection acknowledgement, unique physical channel routing and 10..2000 Hz.
  The 120-channel profile is rejected. No scan starts automatically.
- Adapted the supplied GPL Intan RHX ImpedanceReader algorithm: 30 kHz sample
  clock, integer-period DAC sine, 0.1/1/10 pF series capacitors, at least five
  periods / 20 ms, startup settling, correlation, saturation-aware range choice,
  three-command SPI phase correction, 12 pF parallel correction and RHS 1.1 gain
  correction. Uses the original impedance reader's 0.195 uV ADC scale. Software
  notch/band-pass nodes are not applied to this separate test acquisition.
- Only one selected physical stream receives auxiliary impedance commands at
  a time. All stimulation sequencers/manual triggers are disabled by initial
  acquisition-session setup. No stimulation preset or Trigger is used. Finite
  native runs check USB headers and timestamp continuity and have timeouts.
  Completion/cancellation restores Zcheck off and flat/powered-down test DAC;
  cleanup exceptions retain an error instead of claiming successful cancellation.
- A hardware-operation mutex and running/cancelling state exclude new scans,
  acquisition and playback. The UI disables incompatible actions, reports actual
  quantized frequency, channel progress, magnitude, phase and quality, and offers
  an expanded results grid and CSV with physical addresses and source labels.
  Emergency Stop and app shutdown also signal the impedance worker to cancel.
- 126 Rust workspace tests, eight frontend groups, Clippy with warnings denied
  and native tests passed. Native synthetic fixtures verify correlation with
  phase/DC offset, integer frequency, range/quality logic and known parallel-RC
  correction including Intan's empirical factor. Rust tests cover all 64 route
  indices, invalid channels/frequencies, confirmation and operation exclusion.
  A pre-cancelled request returns before bitfile validation or native device open
  and cannot report any measured channels.
- Synthetic 64-result UI fixtures at 1280x720 and 1460x920 rendered magnitude,
  phase, progress, scrolling and expansion without result text overflow or browser
  errors. Preview is labelled synthetic and refuses real-measurement requests.
- No real impedance test signal was injected during this implementation. This
  is not a measured known-resistor accuracy test or a live cancellation/cleanup
  validation. Comparison with RHX on identical loads and frequencies, all-channel
  measurement accuracy and post-cancel hardware recovery remain pending.

## 2026-10-23 Full-Rate Spike Analysis

- Added a separate all-input-channel candidate-spike analyzer before display
  decimation. Signed uV thresholds, refractory periods, pre/post windows and an
  optional dedicated 300-3000 Hz band-pass are configurable while stopped.
- Retains crossing/peak sample indices, source time, physical stream/chip channel,
  signed peak, peak-to-peak voltage and complete full-rate snippets. Gap/artifact
  resets and session-end incomplete snippets are explicit, not interpolated.
- Backend continuous CSV uses create-new semantics and reports open/write/finalize
  failures without stopping acquisition. Bounded event polling reports UI losses;
  cached CSV export is distinct from a durable session record.
- Spike tab now has raster, per-channel waveform and numeric-event views. Activity
  spike rates and statistics use these events instead of the 200-entry log. Spike
  statistics snippets no longer use min/max display bins. Playback seeks/restarts
  and pipeline changes reset the UI analysis segment without reusing CSV event IDs.
- 136 Rust workspace tests and nine frontend test groups pass; Clippy passes with
  warnings denied. New tests cover all 16/64/120 software channels, exact numerical
  peaks, full-rate filtering, cross-block snippets, refractory/polarity, gap/artifact
  rejection, cursor eviction, segment resets, CSV contents/overwrite protection and
  a forced write failure that leaves detection operational.
- Browser UI interaction tests use synthetic data only. Verified 64 channel tiles,
  data values, calibrated axes and scrollable waveforms. Fixed early grid items
  being drawn before the final grid layout, which previously squeezed canvas text.
- No real stimulation or impedance signal was emitted for these checks. Biological
  detection sensitivity/specificity and sustained USB + recording + UI performance
  remain unvalidated. Threshold crossings are not sorted neuronal units.

## 2026-10-29 MEA 64 Display Mapping

- Corrected array-site mapping: sites 0..31 use B port with the same zero-based
  channel number; sites 32..63 use C port with channel = site - 32. Preserved the
  8x8 physical site layout; its second row ends in 52, 53.
- Display names are B-00..31 / C-00..31; stable command/storage IDs remain
  B-000..031 / C-000..031. Acquisition lane order, FPGA/native code and physical
  stimulation/impedance routes were not modified.
- Activity and selection arrays show both site number and port-local name.
  Waveform labels, channel selectors, activity inspector and shared profile users
  receive the same names. Default labels in legacy settings are upgraded; custom
  aliases/colors/enabled states are retained. Old saved array coordinates rebase
  to layout version 2, including recovery snapshots and loaded experiment presets.
- All nine frontend test groups pass. Tests verify every cell's site-to-data-index
  relationship with distinct synthetic per-channel amplitudes, bijective inverse
  mappings, both 52/53 locations, stable physical routing, legacy-settings recovery,
  migration idempotence, and current-version layout editing. Browser checks confirm
  9 -> B-09/data lane 9, 62 -> C-30/data lane 62, activity selection B-09,
  and waveform pagination through C-16..C-31. UI data are synthetic, not a new
  electrical channel-mapping measurement. No hardware test signal was emitted.

## 2026-10-29 Single-Channel Waveform View

- Added display-only single-channel focus with a full-height trace, uV amplitude
  ticks and source-sample-based relative time axis. Focus controls are separate
  from acquisition and output commands. Channel names use the shared MEA profile.
- Entry points: waveform mode controls, double-click a lane/label, or the activity
  inspector. Supports channel selection and previous/next navigation, amplitude
  fitting of the focused channel only, time-window controls, display pause and
  return to the unchanged overview page. Workspace recovery retains focus mode.
- Nine frontend test groups pass. Added tests cover 16/64/120-channel focus
  boundaries, retained page/visibility/gain/frame references, a full-height trace
  instead of multiple lanes, fitting a hidden focused channel, non-mutated sample
  buffers, and display pause. Existing routing and recording-related tests pass.
- Synthetic browser checks verified B-09 and C-30 selection, synced amplitude and
  focus selectors, 500 ms timing, first/last-channel disabled navigation, label
  double-click, activity-map entry and focus recovery after reload. While display
  was paused the preview backend stayed ACQUIRING. No hardware signals were sent.
- Focus uses display min/max bins; it does not restore sample detail lost to
  display decimation. Biological signal quality and hardware performance were
  not measured in these checks.

## 2026-10-29 Click-To-Enter Channel Details

- A single click on a waveform lane or channel label enters that channel's
  full-height view. A separate visibility checkbox retains overview hide/show
  controls. Clicking inside the detailed plot does not unexpectedly exit it.
- Detail actions use the same channel for amplitude, spectrum/spectrogram and
  spike statistics/snippets; both analysis views have a return-to-waveform action.
- All nine frontend test groups pass, including new page-aware lane hit tests,
  out-of-bounds clicks, label entry and independent visibility control tests.
- Synthetic browser acquisition verified B-09 label entry, B-24 lane entry on
  page 17-32, analysis selector synchronization, continued sequence increments,
  amplitude fitting, 500 ms display timing and return to page 17-32. At 1024x768
  the detail toolbar did not overflow; the plot retained 445 px height. Browser
  error/warning logs were empty. No hardware output or new electrical test was
  performed. These remain display envelopes, not recovered full-rate samples.

## 2026-10-29 Waveform Continuity And Display Buffer

- Compared local Intan waveformdisplaymanager.cpp: compressed views draw a
  min/max line per screen column with a one-device-pixel cosmetic pen; sufficiently
  zoomed views use polylines. Our previous renderer drew independent envelopes
  without joining bins and distributed overview frames by point count. The
  desktop poll returned only the newest frame, losing intermediate display blocks
  when the UI was slower than the producer. These were display-path defects, not
  evidence that the raw recorder had dropped the same samples.
- Added cursor-based poll_waveforms with bounded source-time history and a
  generation for start/seek/source resets. The legacy latest-frame command remains
  available. Added exact sample counts and first/last sample endpoints per bin,
  including a partially filled final bin. Raw acquisition, recording and stimulus
  scheduling were not changed.
- Overview/detail use one source-time coordinate system. Pixel envelopes retain
  extrema, contiguous samples are connected, and genuine gaps/flagged boundaries
  remain disconnected. Display-gap indication is distinct from capture metrics.
  A paused or slow UI can recover retained frames without duplicate processing;
  eviction outside the bounded history is explicit. Spectral history cannot span
  a missing interval. Display envelopes are still not full-rate reconstruction.
- All 139 Rust tests, nine frontend groups and warning-free Clippy pass. New tests
  cover multi-block polling/cursors, eviction/reset, time retention, partial bins,
  pixel extrema, endpoints, honest gap spacing, flags, and spectral gap resets.
- Browser checks with 64 synthetic channels and a 1-second window show continuous
  traces, a hidden gap badge for a continuous source, and no browser warnings or
  errors. The screenshot is simulated data; hardware acquisition/noise quality
  was not measured in this update. No hardware stimulus was emitted.

## 2026-10-29 NeuroX Studio Product Branding

- Renamed the window/product metadata, header, reports, diagnostic exports and
  startup messages to NeuroX Studio. The source node uses a neutral acquisition
  label. New recording/export names use the neurox prefix; original .rhsraw files
  remain readable and existing user-selected paths are not rewritten.
- Kept the application identifier and legacy local-storage keys to retain channel
  mappings, presets and workspace recovery. Actual RHS2116 hardware identification,
  the recording protocol/format and third-party license notices remain accurate.
- Packaging emits NeuroX Studio.exe. New installs use the NeuroX Studio directory;
  upgrades preserve the legacy working directory. Desktop/Start menu shortcuts
  use NeuroX Studio, and matching legacy shortcuts are backed up before removal.
- Nine frontend test groups pass, including product-name consistency, preserved
  configuration identifiers and new default recording-name checks. The installer
  passes PowerShell syntax validation. This rename does not send hardware commands.

## 2026-10-29 Layout And Per-Channel Voltage Ranges

- Removed event-console markup, rendering, controls, styles and its reserved grid
  row. The bounded diagnostic event history remains for exports/analysis. Added
  independent accessible left/right panel toggles, saved in workspace recovery and
  experiment display settings. Enlarged common text, controls and channel labels;
  toolbar controls wrap instead of competing with the navigation tabs.
- Added lower/upper absolute uV limits per profile and stable channel ID, including
  asymmetric ranges. Invalid/nonfinite/reversed limits are rejected. Applying,
  resetting and fitting affect only display state; raw samples, ADC conversion,
  overview gain and hardware output are unchanged. Paths are clipped at the range
  bounds instead of clamped to artificial plateaus. Display clipping and ADC rail
  saturation have separate indicators.
- Nine frontend groups pass, with tests for removed console controls, manual
  range validation, calibrated endpoints/midpoint, asymmetric mapping, geometric
  clipping, unchanged samples and overview gain independence.
- Browser checks: 64-channel synthetic acquisition continued as panels collapsed;
  plot width expanded from 562 to 850 to 1178 px. B-09 retained -20..20 uV and B-10
  -10..50 uV independently, including after reload. Invalid ranges left the applied
  range unchanged. Reset and spectrum navigation worked. At 1040x760 with both
  panels open there was no page/toolbar/range-form horizontal overflow, and the
  plot retained 334 px height. Browser warning/error logs were empty. These are
  display tests, not biological-signal or hardware-output qualification.

## 2026-10-29 Impedance Probe Map And Device Tools

- Removed the digital/analog I/O and local-status API tool sections, their DOM
  callbacks and obsolete layout styles. Retained impedance and audio monitoring
  as full-width sections, plus existing scan isolation, cancellation, explicit
  AC confirmation and stimulation interlocks. Native internal services were not
  broadened or automatically started.
- Impedance scan requests, array cells and CSV exports now follow the shared
  configured Probe Map. MEA 64 uses an 8x8 array: left sites map directly to B,
  right sites minus 32 to C. First row: B-09/B-08/B-05/B-01/C-30/C-26/C-23/C-22.
  Stable channel IDs bind result values; response order does not assign positions.
  Source profile metadata prevents rebinding old results to a newly selected MEA.
- Physical stream/chip addresses are checked against the channel ID; mismatches
  are explicitly marked instead of shown as trusted values. CSV retains raw
  results and adds display/site/row/column metadata. Partial results leave array
  positions stable; disabled electrodes are excluded from scan requests. Unknown
  and duplicate results remain represented in CSV for diagnosis.
- Nine frontend groups, all 140 Rust tests and warning-free Clippy pass. New tests
  assign distinct values to all 64 physical channels, reverse response ordering,
  verify every site/coordinate/address/value, validate CSV order, partial results,
  aliases/disabled cells, mismatch/duplicate/unknown handling, 16/120 layouts and
  native conversion of the first map row to indices 9/8/5/1/62/58/55/54.
- Browser checks verified 64 impedance cells in exactly the same ID order as the
  activity map, only two remaining tool sections, 16/64 switching, expand/restore,
  and no warning/error logs. At 1040x760 the page has no horizontal overflow; the
  array scrolls internally without reflowing electrodes. All visible pending
  cells are unmeasured: no real impedance scan or hardware test signal was run.

## 2026-11-02 Dual-Column Waveforms And Independent Ranges

- The default overview shows 64 channels in two columns, 32 lanes per column.
  MEA 64 keeps B-00..B-31 on the left and C-00..C-31 on the right. The original
  16-channel layout remains selectable; 120-channel sources use 64-channel pages.
- Each stable channel ID has its own absolute microvolt range in both overview
  and single-channel detail. Limits, resets, asymmetric ranges, clipping warnings
  and session recovery use the same settings without modifying sample data.
- Waveforms, labels, pointer hit tests and spike dots share column/lane geometry.
  Narrow or short surfaces scroll internally instead of crushing channel labels.
- All 11 frontend test groups pass. Optional Playwright regression
  `npm run test:waveform-browser` requires Node 20+, Playwright and a running
  synthetic preview (default http://127.0.0.1:1421/). It refuses hardware backends.
  This test passes for 1920x1080, 1460x920 and 390x844, including pixel checks,
  right-column entry, independent B-09/C-09 ranges, invalid-input isolation,
  reset, reload recovery, pagination and screenshots without page errors.
- Release builds embed the updated frontend. No acquisition, recording, routing,
  stimulation or device protocol code was changed or exercised on hardware.
- Added explicit apply-to-all and reset-all commands for the current source
  profile. They share input validation with individual ranges, include channels
  on other pages, preserve other profiles and persist through session recovery.
  Playwright checks pass for all 16/64/120 entries, invalid-input non-mutation,
  individual overrides after bulk application, focused detail and isolated reset.

## 2026-11-05 Hardware Deployment Bundle

- Adds the exact board-provided lab bitfile and unmodified FrontPanel 5.2.10
  amd64 driver directory to an opt-in binary package. Seven hardware payload
  hashes are pinned in `scripts/hardware-payload.json`; both driver catalogs
  verify as signed by Opal Kelly Incorporated. SDK/private files stay out of Git.
- Driver-helper dry runs accept the complete payload and reject missing files
  or invalid arguments without installing a driver. Normal installation requires
  administrator rights and explicit confirmation, uses only USB INF installation,
  and does not force replacement, delete drivers, reboot or alter security policy.
- `scripts/Test-ReleasePackage.ps1 -Version 2026.11.05-hardware1` verifies every
  runtime ZIP entry against manifest sizes/hashes and the unpacked payload,
  pinned hardware hashes, catalog signatures, helper dry run, source file list
  and archive checksums. It also supports the original minimal release policy.
- The publisher's Windows PowerShell 5 mock regression covers argument boundaries,
  initial draft creation, upload, downloaded-file checksums, publication gating
  and retry. Mock results are not evidence of a real GitHub publication.
- The application EXE is unchanged from 2026.11.05. This deployment change does
  not install drivers, open boards, load FPGA firmware or send stimulation during
  validation. A fresh-PC installation and physical output qualification remain
  untested; neither a valid file hash nor driver signature proves hardware output.
