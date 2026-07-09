# OE (Open Ephys) vs KeyVast GUI — 原始数据一致性对比

**日期:** 2026-07-09  **数据:** `E:\0708data`（同一只小鼠、同一状态的两段录制）
**基准 (标准答案):** Open Ephys `2026-07-08_22-02-13`
**被测:** KeyVast GUI `session_20260708_141507\recording.kvraw`

## 结论 (TL;DR)

**幅度标定、通道映射、高频/spike 频段、工频噪声环境 —— 完全一致。**
**唯一系统性差异：我们的 GUI 对低频多做了一级高通（RHD 片上 DSP 失调消除开启 + 模拟低截更高），
OE 没有。** 因此我们的数据 DC 失调被去掉、且 <~4 Hz 的 LFP/delta 被明显衰减；≳10 Hz 与 OE 几乎重合。

这不是文件写入 bug（`.kvraw` 忠实保存了芯片输出），而是**采集配置差异**。

> ✅ **本 PR 已把该配置改为与 OE 一致**（关闭 DSP 失调消除 + 模拟低截降到 0.0955 Hz）。
> 详见下方「已改为与 OE 对齐」。文中的 `.kvraw` 是**改动前**录制的，故仍带此差异。

## 两份数据基本情况

| | OE (基准) | KeyVast GUI |
|---|---|---|
| 格式 | `continuous.dat` (int16 交织) | `recording.kvraw` v2 (int16 交织) |
| 采样率 | 30 kHz | 30 kHz |
| 通道数 | 32 | 32 |
| 标定 | `bit_volts = 0.195 µV/count` | `RHD_AMPLIFIER_MICROVOLTS_PER_COUNT = 0.195` ✅ 相同 |
| 时长 | 640 s (19,207,936 样本/通道) | 850 s (25,514,496 样本/通道) |
| 完整性 | sample_number 连续 | packet 9907–109572 连续、`clean_stop=true`，无丢包 |

> 两段是**先后各录一段**（时长不同、非逐样本同步），因此对比用统计/频谱/通道指纹，而非逐点相减。

## 关键证据

### 1. 频谱 (决定性) — `fig1_psd.png`
- **≳100 Hz 到 15 kHz：两条 PSD 曲线几乎完全重合，KV/OE 功率比 ≈ 1.0**（1–9 kHz 中位比 **0.95**）。
  → 增益/标定/高截/采样率全部一致，无缩放 bug。
- **50 Hz 工频峰两边都有且幅度相当** → 同一电气环境、标定一致。
- **<~10 Hz：KV 相对 OE 明显下滚**，半功率(-3 dB)拐点 ≈ **3.7 Hz**，<2 Hz 处功率比降到 ~0.2。

### 2. 逐通道 DC 失调与幅度 — `fig2_perchan.png`
- **DC 失调：** OE 每通道有 ±(几十~160) µV 的固有失调；**KeyVast 全部 ≈ 0**（DSP 数字高通把 DC 去掉了）。
- **std/RMS：** OE 中大的通道 (2/6/10/14/28) 在 KV 里同样相对偏大 → **通道映射一致，无错位/交换**。
  差异最大的正是这些"低频占比高"的通道，符合"低频被高通削掉"的解释。
- 全局 std 中位比 0.75（被低频主导），但这是低频差异所致，非增益差异。

### 3. 通道间相关结构 — `fig3_corr.png`
OE 平均 |相关| 0.85，KV 0.68；两个相关矩阵之间相关 0.74。KV 偏低是因为高通削掉了各通道共有的低频共模成分，方向自洽。

## 根因 (配置差异)

OE `settings.xml`（标准答案）：
```
LowCut = 0.0955 Hz   HighCut = 7604 Hz   DSPOffset = "0"   (片上 DSP 失调消除【关闭】)
```

我们的 GUI（**改动前**，[`crates/kv-rhd/src/commands.rs`](crates/kv-rhd/src/commands.rs)）：
```rust
dsp_en: 1,                       // 片上 DSP 失调消除【开启】  ← 差异①
...
registers.set_dsp_cutoff_freq(1.0);   // DSP 高通 ~1 Hz(实测有效拐点 ~3.7 Hz)
registers.set_upper_bandwidth(7_500.0); // 高截 ≈ OE，一致 ✅
registers.set_lower_bandwidth(1.0);     // 模拟低截 1.0 Hz vs OE 0.0955 Hz ← 差异②
```

即低频侧有**两处**比 OE 更"狠"的高通：① DSP 数字高通开启（主因，去 DC + 削 <4 Hz）；② 模拟低截 1.0 Hz 比 OE 的 0.095 Hz 高一个数量级。高截/增益/采样率均已对齐。

## 已改为与 OE 对齐（本 PR 改动）

本 PR 把 RHD 采集配置改成与 OE 参考录制一致，共三处：

| 文件 | 改动 | 对齐 OE |
|---|---|---|
| `crates/kv-rhd/src/rhythm_board.rs` | `enable_dsp(true)` → **`false`**（**真正下发芯片的地方**，会覆盖 commands.rs 默认） | `DSPOffset=0` |
| `crates/kv-rhd/src/commands.rs` | 默认 `dsp_en: 1` → **`0`** | 让默认与配置路径一致 |
| `crates/kv-rhd/src/commands.rs` | `set_lower_bandwidth(1.0)` → **`0.0955`**（`<0.15 Hz` 会启用 3 MΩ 档，RL DAC 饱和） | `LowCut=0.0955` |

金标准测试 `crates/kv-rhd/tests/rhd_command_lists.rs::default_registers_match_open_ephys_rhd_30khz_settings`
的期望寄存器同步更新为 `reg4=140 / reg12=127 / reg13=255`（逐字节核对确为 OE 语义）。全 workspace 测试通过。

> **权衡（已知并接受）：** 关闭 DSP 高通后，放大器的 DC 失调（±几十 µV）会重新出现（与 OE 一致）。
> 已验证其远离 0x4000 半幅门限，不影响采集初始化的居中检测；0.0955 Hz 模拟低截仍会挡住真正的直流漂移。
>
> **验证下一步：** 用改后代码重录一段（同鼠同状态），`wave1_lowfreq` 里 KV 的基线/慢波应与 OE 一致。

## 波形可视化（肉眼对照）
窗口非时间对齐（OE @320 s / KV @425 s，各取录制中段），因此单个事件/尖峰不同属正常，
看的是**系统性差异**。此 `.kvraw` 为**改配置前**（DSP 高通开启）录制。
- `22-oe-vs-gui-data-compare/wave1_lowfreq.png` — **主差异**：12 s、~1 kHz、保留直流。OE 有基线失调(虚线均值)+慢波起伏；KV 基线贴 0、慢波被削平。
- `22-oe-vs-gui-data-compare/wave2_multichan.png` — 32 通道概览(2 s, 去直流)：通道分布/活跃度对照，两边同样的通道活跃。
- `22-oe-vs-gui-data-compare/wave3_zoom.png` — 150 ms 细节：50 Hz 起伏与快活动/尖峰形态两边一致（>100 Hz 一致，印证增益标定相同）。

## 300 Hz 高通对比（剥掉 LFP，只看 spike/MUA 频段）
把两边都做 4 阶 Butterworth 300 Hz 零相位高通(`sosfiltfilt`)，去掉差异所在的低频后对比高频。
- `22-oe-vs-gui-data-compare/hp300_perchan.png` — 逐通道 spike 频段 RMS。呈**双峰**：
  - **奇数通道(安静/参考，无本地放电)**：OE≈8.5 µV、KV≈6.8 µV，比值恒为 **0.80** → KV 噪声底反而低 ~20%。
  - **偶数通道(有真实 MUA/放电)**：比值 0.89–1.54、均值 **1.06** → 信号幅度基本一致(KV 偶尔更高)。
- `22-oe-vs-gui-data-compare/hp300_wave.png` — 300 ms spike 频段波形。ch1 纯噪声 hash；ch6/ch10 清晰的双相锋电位，**形态/时间常数/噪声底两边一致**(点线=-4×MAD 阈)。
- `22-oe-vs-gui-data-compare/hp300_psd.png` — 高通后功率谱通道平均基本重合，>1 kHz 比值贴 1.0。
- 明细 `22-oe-vs-gui-data-compare/hp300_stats.json`。

**结论：** 宽带 0.75 的差异几乎全部来自低频(LFP/DSP 高通配置)；**300 Hz 以上的放电频段，真实信号通道 KV≈OE(比值~1.0)，安静通道 KV 噪声底还略低**。说明我们 GUI 在 spike 频段的采集是忠实的。

## 复现
`scratchpad/compare.py`（全量逐通道统计 + Welch PSD + 相关矩阵）、`plots.py`、`wave.py`（波形出图）。
统计明细见 `22-oe-vs-gui-data-compare/stats_report.json`。
