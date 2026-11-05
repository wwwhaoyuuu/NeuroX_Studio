# RHSRAW 转 Intan RHS

`rhsraw_to_rhs.py` 是独立转换脚本，使用 Python 3.9 或更高版本的标准库。
转换本身不需要 NumPy、Rust、NeuroX Studio、板卡或 Intan 软件。
它生成真正的 traditional Intan RHS v1.0 文件，不是简单修改后缀。
原始 `.rhsraw` 始终只读，现有输出不会被覆盖。

## 使用

在脚本所在目录运行，把示例中的输入、输出替换为实际完整路径。
输出的上级目录须已经存在，请选择空间足够的磁盘，当前不要输出到 C 盘。

```powershell
python rhsraw_to_rhs.py "G:\实验数据\trial.rhsraw" -o "G:\实验数据\trial.rhs"
```

输出三个文件，建议放在一起保存：

- `trial.rhs`：原始电极电压、相对采样时间和可用的数字输入。
- `trial.rhs.json`：原始参数、SHA-256、物理通道对应关系、转换限制及有效采样数。
- `trial.rhs.blocks.jsonl`：每个原始块的绝对采样索引、行号、TTL 可用性和标志。

遇到 `.part` 或 `.INCOMPLETE`，表示转换没有完整完成，不能把它当成成功结果。
重新导出请指定新的文件名，不要覆盖原记录。

## 数据保留与限制

- 放大器 ADC 数值逐点保留；脚本不滤波、不重采样、不改变增益。按通道物理地址排序，
  例如 `B-009`、`C-020`；原文件列号到输出列号的对应表保存在 JSON 中。
- 微伏换算由 RHS 分析读取器完成：`(ADC - 32768) * 0.195`。
- 时间戳减去原记录的首采样索引以适应 RHS int32；原始绝对索引仍保存在伴随文件里。
  缺失时间不会被插值。过长、不能放入 int32 的记录会报错，需要另行拆分。
- 只有所有原始块都有 TTL 时才导出 16 位数字输入；有块缺失 TTL 就不导出数字输入，
  避免把“未知”伪装成“低电平”。原始信息仍在 `.rhsraw` 中。
- RHSRAW 没有完整的逐点刺激电流、顺应性、放大器恢复和电荷恢复状态。
  RHS 格式强制要求的刺激字以零占位，电流比例设为 NaN，因此官方读取器的刺激电流为 NaN。
  **刺激字和布尔恢复状态中的零不是“没有刺激”的证据，不能用于刺激历史分析。**
- DC、板载 ADC/DAC、数字输出没有记录就不生成这些通道。硬件滤波和阻抗未知项为 NaN；
  必填离散头字段是兼容性占位值，不能用于还原真实硬件设置。
- 自定义 probe 几何、处理链和其他 NeuroX 参数留在 JSON 中，不保证其他软件自动恢复。

RHS 每块需要 128 个点。不是整块时默认报错，不丢弃、不补造数据。
只有明确接受合成尾段时才使用：

```powershell
python rhsraw_to_rhs.py "G:\实验数据\trial.rhsraw" -o "G:\实验数据\trial.rhs" --pad-tail
```

此选项重复最后一个 ADC/TTL，并延续时间戳。分析时必须只取 JSON 的
`validSamplesPerChannel` 个真实点，排除 `paddedSamplesPerChannel` 个合成点。

## 后续分析

转换后的文件可以交给 Intan RHS 数据读取脚本，再接滤波、spike detection、频谱等分析。
这不等同于保证任意第三方软件或 RHX 界面的所有功能都能恢复。

- 文件格式规范：https://intantech.com/files/Intan_RHS2000_data_file_formats.pdf
- 官方读取工具：https://intantech.com/downloads.html
- 官方 Python 示例：https://github.com/Intan-Technologies/load-rhs-notebook-python

## 验证

```powershell
python test_rhsraw_to_rhs.py -v
```

内置测试不需要第三方包，覆盖 16/64/120 通道、排序、CRC、footer、时间断点、
TTL 缺失、尾段处理和禁止覆盖。设置 `INTAN_RHS_READER` 为官方
`importrhsutilities.py` 路径后，会额外做官方读取器往返测试，需要 NumPy。
测试环境若没有 Matplotlib，只为未使用的绘图导入提供占位模块；不修改或模拟数据解析函数。

本次校验的官方读取器 Git blob：`69bc1a2f008ebbff80fa93aa8a887dae9f95278f`。
官方读取器原文件未修改，仅用于本地兼容性测试，不包含在本工具包中。
