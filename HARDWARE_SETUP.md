# 同款硬件部署

适用包：`NeuroX-Studio-2026.11.05-hardware1-windows-x64.zip`。
仅用于本实验室相同的控制板、FPGA 和配套 16/64 路头端；不是所有采用 RHS2116
芯片的设备都能通用。120 电极布局仍仅支持显示/预览。

## 新电脑首次使用

1. 使用 Windows 10/11 x64，下载硬件 ZIP 并**完整解压**到本地可写文件夹。
   不要只拿 EXE，也不要在压缩包里直接运行。
2. 如果尚未安装，安装微软 [WebView2 Evergreen Runtime](https://developer.microsoft.com/microsoft-edge/webview2/)
   和最新 [Visual C++ v14 x64 运行库](https://aka.ms/vc14/vc_redist.x64.exe)。
   这两个微软安装程序不在 ZIP 中；离线电脑需提前准备 WebView2 x64 Standalone Installer
   和 VC++ x64 安装程序。安装完成后，日常采集无需互联网。
3. 首次连接板卡且尚无可用驱动时，右键包内的 `Install-USB-Driver.cmd`，选择
   **以管理员身份运行**，阅读提示后按 Y。它只通过 Windows PnPUtil 安装
   `drivers/amd64/okusb.inf`，不会删除旧驱动、强制降级、自动重启或关闭安全检查。
   有更新且可用的官方驱动时不必降级。如果 Windows 拒绝此旧版驱动，保留报错，
   向板卡提供方或 [Opal Kelly](https://pins.opalkelly.com/) 获取兼容的更新版本，勿关闭签名验证。
4. 保持 `ConfigRHSController.bit` 和 `okFrontPanel.dll` 在 EXE 同目录。
   固件已经包含，不必再从 Intan 中复制。不要换用其他来源的同名文件。
5. 关闭 Intan RHX、其他 NeuroX 实例等可能占用板卡的软件。连接板卡供电和
   **数据 USB** 接口，仅接 POWER ONLY 接口无法通信。
6. 普通权限打开 `NeuroX Studio.exe`，扫描设备，核对检测到的 16/64 路及 Probe Map，
   再开始采集和记录。如果本机保留过无效的 FPGA 路径，在 FPGA 配置中选择本包的 bit 文件。

不需要安装 Rust、Cargo、Node.js、Python、VS Code、Intan 完整软件或完整 FrontPanel SDK。
软件不会因安装驱动或打开界面而自动解锁/触发刺激。刺激使用前仍应完成
[台架验证](STIMULATION_BENCH.md)；安装依赖不等于物理输出或生物实验验收。

## 包内硬件文件

- `ConfigRHSController.bit`：沿用本实验室板卡所配固件，SHA-256 为
  `a93911b1d49d7c51e922cbbaa9aa0241690e845e5b3e3682ef833db95b5016f3`。
  不代表适配任意 FPGA 型号，也不包含永久烧写工具。
- `drivers/amd64/`：FrontPanelUSB 5.2.10 的原始 x64 驱动目录，保留 INF、签名 CAT
  和两个配套 coinstaller DLL；USB INF 的 DriverVer 是 `04/17/2012, 2.3.0.1`，
  不应与 API DLL 的 `5.2.10.0` 混淆。UART 文件一并保留，但安装脚本只安装 USB INF。
- `hardware-payload.json`：来源、适用范围及原始文件哈希；`manifest.json` 覆盖发布包内文件。
  原厂许可见 `licenses/FrontPanel-LICENSE.txt` 和 `THIRD_PARTY_NOTICES.md`。

## 排查与验证范围

可运行 `Check-Dependencies.ps1` 做只读环境检查；可在命令提示符中运行
`Install-USB-Driver.cmd --check` 仅检查驱动文件是否齐全，**不会安装**。
后者不检查签名或设备在线状态，不能代替安装及扫描结果。

本包已做文件完整性、驱动目录签名和打包校验；未在每位使用者的全新电脑上安装验证。
同款硬件还可能受 USB 线缆/端口、供电、Windows 驱动策略或其他软件独占影响。
遇到 `no FrontPanel device detected`，先检查上述连接与驱动；遇到 bitfile 报错，
先检查完整解压和 FPGA 路径。不要通过关闭 Windows 安全保护解决安装问题。
