# Iteration 4 台架刺激验证协议

## 目标

只在非生物测试负载上验证 Rust 安全服务发出的有限双相刺激，记录实际幅值、极性、相宽、相间隔、周期、脉冲数和 Emergency Stop 停止延迟。

## 开始条件

以下项目必须全部确认，任一未知时不执行真实输出：

- [ ] 已根据 RHS2116/headstage 原理图确认刺激输出端、回流端和通道 0 的实际接脚；
- [ ] 已接入老师或实验室批准阻值与功率等级的非生物测试负载；
- [ ] 示波器或电流测量设备的接法、量程、带宽和参考地已确认；
- [ ] 板卡上没有动物、电极或其他生物样本；
- [ ] `stim-session-check` 和随后 `scan` 均通过且 pipe error 为 0；
- [ ] 操作员、日期、板卡序列号、bitfile 路径与哈希已记录；
- [ ] 老师已确认本协议使用的台架参数，或明确批准仅用于低幅台架输出验证。

不要根据板卡照片猜测输出接脚或负载阻值。接法必须来自匹配硬件版本的原理图/连接表。

## 固定预设

| 预设 | 通道 | 电流步进 | 幅度步数 | 相宽 | 相间隔 | 周期 | 次数 |
|---|---:|---:|---:|---:|---:|---:|---:|
| `bench-single-v1` | 0 | 10 nA | 1 | 6 steps / 200 us | 3 steps / 100 us | 3000 steps / 100 ms | 1 |
| `bench-train-v1` | 0 | 10 nA | 1 | 6 steps / 200 us | 3 steps / 100 us | 3000 steps / 100 ms | 5 |

时间换算按当前 30 kHz FPGA timestep。以上是台架验证预设，不是生物实验参数。

## 执行顺序

1. 运行 `scripts\dev-cmd.bat stim-session-check`，保存完整输出。
2. 示波器开始捕获后，仅运行一次 `bench-single-v1`。
3. 检查是否出现且仅出现一个有限双相脉冲；未通过时停止，不运行脉冲串。
4. 保存单脉冲截图和游标测量值，并记录测量通道、量程、探头倍率与负载。
5. 在单脉冲通过后，运行一次 `bench-train-v1`，核对周期和脉冲数。
6. 使用固定 150 ms 入口另做一次活动 Emergency Stop 测试，测量从软件请求到输出停止的延迟；代码返回成功不能替代这项实测。
7. 每次测试后运行只读 `scan`，确认设备仍可打开且 pipe error 为 0。

真实输出命令要求两个独立确认：

```powershell
.\scripts\dev-cmd.bat stim --preset bench-single-v1 --operator NAME --confirm-dummy-load --confirm-measurement-ready
.\scripts\dev-cmd.bat stim --preset bench-train-v1 --operator NAME --confirm-dummy-load --confirm-measurement-ready
.\scripts\dev-cmd.bat stim --preset bench-train-v1 --operator NAME --confirm-dummy-load --confirm-measurement-ready --emergency-stop-after-ms 150
```

## 立即停止条件

- 输出不是有限双相波形；
- 极性、相宽、间隔或次数与预设不一致；
- 输出持续时间超过预设脉冲串；
- USB pipe error 非零、设备掉线、程序 Fault 或板卡异常发热；
- 测量设备饱和、接地方式不确定或接线松动。

## 证据记录

| 字段 | 记录值 |
|---|---|
| 日期/操作员 | |
| FrontPanel 序列号 | |
| bitfile 路径/SHA-256 | |
| RHS2116 端口/stream/通道 | |
| 负载型号/阻值/容差/接法 | |
| 测量设备/探头/倍率/量程/带宽 | |
| 预设 ID | |
| 软件完整输出 | |
| Phase 1 幅值/极性/相宽 | |
| 相间隔 | |
| Phase 2 幅值/极性/相宽 | |
| 周期/脉冲数 | |
| cancel 请求时间/输出停止时间/延迟 | |
| 波形截图路径 | |
| 结论与批准人 | |

Iteration 4 只有在单脉冲、有限脉冲串和活动 cancel 的仪器结果均有对应证据后才能标记完成。
