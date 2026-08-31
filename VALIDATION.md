# RHS Rust Control 验证记录

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
