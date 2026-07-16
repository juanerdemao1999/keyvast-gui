# 20 · GUI 信号展示方法专项审查

> 审查日期：2026-07-07　|　分支：`fix/rhd-miso-delay-chipid`　|　范围：kv-gui 信号展示通路
>
> 通路：硬件/模拟块 → 可选每通道 biquad 滤波 + CAR（`app.rs`）→ `DisplayRing` 摄入期抽取（`disp_ring.rs`）→ `waveform.rs` 堆叠波形渲染；分支：`fft_panel.rs`（FFT）、`spike_overlay.rs`（spike 叠加）、`multiview.rs`（多视图瓦片）。
>
> 方法：8 维度并行深审（ring-datapath / waveform-render / dsp-filters / fft-spectrum / spike-overlay / multiview-layout / perf-realtime / settings-bounds）→ 每条发现由独立『对抗式验证者』对照真实源码核实 → 去重。原始 33 条全部通过验证(0 驳回),去重为 **27 条**。叙述为中文,技术描述保留精确措辞。

## 修复进度(2026-07-07 本次会话)

本次已按优先级逐条修复(`cargo test -p kv-gui` 59 测试全过,`cargo clippy --all-targets` 干净,`cargo fmt` 已应用):

| 状态 | 编号 | 一句话 |
|---|---|---|
| ✅ | **S1** | FFT 改从全速率 `block_history`/`filtered_history` 取样(`fft_samples`),不再用 peak-hold 环 → 无非线性/混叠,谱达全 Nyquist |
| ✅ | **M1** | `history_capacity` 按显示环时间跨度动态设定并钳制,切换滤波器不再把回溯窗砍到 ~21s |
| ✅ | **M2** | 采样率变化(通道数不变)时也重建用户滤波链系数 |
| ✅ | **M3** | spike 不应期改为**时间**判定(用点时间戳)+ 噪声 σ 改用 **MAD/0.6745** 鲁棒估计;新增 4 个单测锁定窗宽无关性 |
| ✅ | **M4** | PSD 归一化改用窗功率 Σw²(消除 +1.76dB) |
| ✅ | **M5** | "Log scale" 开关真正切换 dB↔线性数据域;谱内部改存线性功率 |
| ✅ | **M6** | spike 叠加把平均绝对值 EMA 按 √(2/π) 换算成真 RMS |
| ✅ | **M7** | `snippets_for` 越界返回空;`draw_spike_overlay_pane` 修剪失效通道选择 |
| ✅ | **L2** | CAR 改在 f64 域完成、末尾单次量化(消除预滤波硬削)|
| ✅ | **L3** | HP 加 Nyquist 上限守卫(与 LP 对称)|
| ✅ | **L4** | FFT 时间平滑 EMA 改在线性功率域 |
| ✅ | **L7** | LFP 带瓦片不再显示 spike 阈值叠加(AP 带保留)|
| ✅ | **L9** | 主波形标签页通道范围改从 `display.visible_channels` 派生;移除死字段 |
| ✅ | **L11** | `filter_block_with_chains` 单次克隆(与 L2 合并)|
| ✅ | **L13** | 修正 waveform.rs "painter 无分配"失实注释 |
| ✅ | **L14 / I4** | config 对 `visible_channels` / `channels_per_group` 钳制到 ≥1 |
| ✅ | **I2** | spike `process_block` 加交织长度校验(防越界 panic)|
| ✅ | **L6** | spike 叠加 σ 从首个真实样本播种,消除 ~150ms 启动瞬变 |
| ✅ | **L10** | 新增 `warm_band_ring`:带瓦片打开时从历史回填,消除左侧空白 |
| ✅ | **L12** | FFT 仅在数据(`block_seq`)或参数变化时重算,暂停/帧间不再空转 |
| ✅ | **I3** | `collect_channel` 峰值点改用其自身时间(消除最多 1 桶左偏,对齐防抖已保证无抖动)|
| ⏸ | **L1** | 每窗均值去 DC — 属 Intan RHX 式设计取舍,保留并已在文档说明 |
| ⏸ | **L5** | 两检测器已都基于真 RMS-σ,分歧大幅缩小;完全统一(共享检测器)后续再做 |
| ⏸ | **L8** | 带瓦片独立 amp/time 定标 — 需为 LfpView/ApView 加分区滚动,属功能增强,暂缓 |
| ⏸ | **L15** | 通道 enable 列表按 ch_count — 会让面板列出全部设备通道,属 UX 变更,暂缓 |
| ⏸ | **I1** | 重滤零状态瞬变(历史头部亚毫秒)— 可接受,暂缓 |

> ⏸ = 本次未改动,理由见对应条目。下方清单保留原始定位,便于后续追踪。

## OE 对齐修复(2026-07-07):连续显示形态与回放连续性

针对"回放/连续信号视觉上很奇怪",对照 Open Ephys / Intan RHX / SpikeGLX 源码做了两处根治(`cargo test -p kv-gui` 62 过、clippy 干净、release 构建通过):

### 问题 B — 回放前向间隙 → 环冻结(已修)
- **根因**:`playback::tick()` 每帧只读一个以 cursor 结尾的 `samples_per_channel`(64/256)帧小块,但按帧率 cursor 每帧前进 ≈500 帧 → 跳帧 + 前向间隙块;`DisplayRing::push_block` 的 `s = abs - block_start` 在 `abs < block_start` 时下溢(**实测**:debug panic / release 回绕丢块→**波形冻结**)。
- **修复**:
  - `tick()` 改为读**自上次到当前 cursor 的连续整段**(`[prev_end, cursor)`,上限 `MAX_DISPLAY_FRAMES`)→ 无跳帧、块连续([playback.rs](../crates/kv-gui/src/playback.rs))。
  - `push_block` 加**不连续自愈**:`next_expected` 不落在 `[block_start, block_end]` 时(前向间隙 / 后向或大跳,如 seek)`reset()` 后在新位置重新播种,永不下溢([disp_ring.rs](../crates/kv-gui/src/disp_ring.rs))。
  - 回归测试:`push_block_reseeds_on_forward_gap` / `_backward_jump` / `_stays_continuous_for_adjacent_blocks`。

### 问题 A — 单峰值折线 → OE 式 min/max 填充包络(已修)
- **根因**:主环 `.with_peak_hold()` + 渲染 `collect_channel` 每桶取**单个 |值|-max 点**连成折线——三大参考工具(OE/RHX/SpikeGLX-BinMax)都不这么画,它们保留**每列真 min 和真 max**画填充带。单峰值折线在 +峰/−峰间乱跳,看着像噪声。
- **修复(向 OE 看齐)**:
  - 环每桶改存 **(min, max)**(i16,内存不变、无损);移除 `peak_hold` 标志(三个环统一)。
  - 新增 `collect_channel_band`:每列取 min/min、max/max,并做 **Open Ephys 式跨列桥接**(把本列 [min,max] 拉伸到与上一列重叠)消除毛刺。
  - spike 检测跑在 min 包络上;环内用 O(1) 的 `deque[j]` 索引(避免 `iter().skip()` 的 O(len²))。
  - **填充方式(关键修正)**:最初用 `egui_plot::Polygon` 填充,实测出现"大箭头/楔形"——因为 egui_plot 的 `Polygon` 用 `Shape::convex_polygon`(凸多边形填充),而 min/max 带是**凹的**(基线带 + 向下尖刺),凸填充把尖刺之间填成大三角。**改为在屏幕空间用三角带 `Mesh`** 在 max/min 两条包络间显式三角化填充(凹形正确,每通道一个 mesh,裁剪到绘图区),外加 max/min 描边。已用合成 spiking 信号单测验证包络是"基线+尖刺"而非楔形。

> 建议:`run-gui.bat` 打开(Source=Simulator)目视确认——回放应平滑滚动,连续波形应呈实心包络带而非乱跳折线。

## 0. 已修复的旧问题(相对 docs/19)

审查确认以下旧发现在当前代码中**已修复**,本次不重复列出:

- `C3` FFT 频率轴用错采样率 → 现用 `ring_sr = Fs / RING_DWNSP`（`fft_panel.rs:146`)。
- `H14` hover 幅值读数漏增益项 → 现为 `delta_y * amp_scale / (3*DEFAULT_CHANNEL_SPACING)`,对任意 amp_scale 正确（`waveform.rs:465`)。
- `H15` spike 不应期用整满采样率常量 → 现用 `sample_rate / RING_DWNSP`（`waveform.rs:732`)。**但见下方 `M3`:该修复只解决了常量,更深层的『在渲染抽取后的点上检测』问题仍在。**
- `H18` notch_idx 越界 panic → `config_persist` 现对 notch/amp/time/spacing 全部钳制（`config_persist.rs:303-325`)。

## 1. 严重度汇总

| 严重度 | 数量 | 编号 |
|---|---|---|
| 🟠 High | 1 | S1 |
| 🟡 Medium | 7 | M1–M7 |
| 🔵 Low | 15 | L1–L15 |
| ⚪ Info | 4 | I1–I4 |
| **合计** | **27** | |

**贯穿主题**:大量问题都源自同一根因——**主显示环 `disp_ring` 启用了 `peak_hold`(每 4 样本窗口只保留 |value| 最大者),且渲染时又按 `stride2` 做第二次峰值保持抽取。这个非线性、随时间窗宽变化的抽取被下游当作『均匀采样的真实信号』直接喂给 FFT、spike 计数与阈值估计。** 对堆叠波形本身(纯粹目视)这是合理的显示优化;但任何试图从这些点做**定量科学计算**的分支都因此失真。

---

## 2. 🟠 High

### `S1` [scientific-accuracy] FFT 直接对 peak-hold + 无抗混叠的数据做谱,PSD 不成立
- [ ] **定位**：`fft_panel.rs:149`(输入)/ `app.rs:270`(peak-hold 环)/ `app.rs:1590`(每帧喂入)　**验证**：CONFIRMED　**置信度**：high
- **问题**：`compute_spectrum` 经 `ring.last_n_samples_f64` 从 `self.disp_ring` 取样,而该环是 `.with_peak_hold()` 创建的:每 4 样本窗口只存 |value| 最大的那个样本(`disp_ring.rs:158-186`),这是**非线性的取极值(类整流)选择,不是线性抽取**;且 4× 抽取前**没有抗混叠低通**。两处独立违背 FFT 的均匀采样前提:(1) 取极值是非线性操作,注入信号中本不存在的谐波/互调与低频整流偏置;(2) 无抗混叠时,环 Nyquist(30kHz 下 3750Hz)到硬件 Nyquist(15000Hz)之间的全部能量折叠回 0–3750Hz 显示带。面板却明确标注为标定过的 PSD(轴 `Power (µV²/Hz)`,Hann 归一化、单边加倍、每-count µV 换算一应俱全)。
- **影响**:FFT 面板是操作者唯一的在线频谱质检手段(工频、刺激伪迹、频带功率)。当前绘制的是**失真包络的谱**,而非神经信号谱:高频内容混叠进 LFP/spike 带,非线性造出幻峰,噪声底被抬高。任何关于 50/60Hz 污染或频带功率的判断都不可靠。
- **建议**:不要用 peak-hold 主环喂 FFT。改为:(a) 对原始未抽取通道流做谱、抽取前加抗混叠 FIR;或 (b) 为分析器维护一个独立的**线性抽取**环(结构中已存在非 peak-hold 的 `disp_ring_lfp`,`app.rs:271`,可作参考)。至少将面板改标为『包络谱』而非 PSD。
- **注**:ring-datapath 维度的验证者把此条降为 medium(理由:RING_DWNSP=4 窗仅 ~133µs,对 50/60Hz 相位推进极小,幻峰有限,主要是噪声底抬高);fft-spectrum 维度维持 high。综合取 **High**,因面板对外宣称是定量 PSD 却建立在被破坏的采样假设上。

---

## 3. 🟡 Medium

### `M1` [ui-behavior] 切换/调整任一滤波器会把可回溯历史从 120s 砍到 block_history 深度(~21s demo / ~85s device)
- [ ] **定位**：`app.rs:553`　**验证**：CONFIRMED(high→medium)　**置信度**：high
- **问题**:`disp_ring` 按 `RING_SECS=120s` 分配(`disp_ring.rs:28`,30kHz 下 900_000 槽),实时采集时可独立积累满 120s。但任何滤波设置变化经防抖走 `rebuild_filter_chains → refilter_history`,其中 `self.disp_ring.reset()`(`app.rs:553`)后**只从 `block_history` 重建**(`app.rs:560-562`),而 `block_history` 上限固定 `history_capacity = 10_000` 块(`app.rs:258`)。demo(64 样本/包,30kHz)= 21.3s;device(256 样本/包)= 85.3s,均 < 120s。
- **影响**:用户暂停查看某瞬变事件、再调一个显示滤波器想看清,历史缓冲随即从最多 120s 塌缩到 ~21s/~85s,更早数据丢失且不可恢复。号称的 120s 浏览窗在任何滤波交互后实际不可达。仅影响显示(录制始终为原始数据)。
- **建议**:让原始保留深度 ≥ `RING_SECS`——`history_capacity` 按秒数而非固定块数设定(≥120s),或从与环等深的原始缓冲重建,或在环自身已存跨度上原地重滤而非 reset+重放 block_history。

### `M2` [scientific-accuracy] 仅采样率变化(通道数不变)时,用户显示滤波器系数不重建 → 截止频率错误
- [ ] **定位**：`app.rs:612`(重建条件)/ `app.rs:617-625`(Fs 变化分支漏了用户链)　**验证**：CONFIRMED(high→medium)　**置信度**：medium
- **问题**:`ingest_block` 仅在**通道数变化**(`filter_chains.len() != ch_count`)或 FilterSettings 变化(`update()` `app.rs:1596`)时重建用户 `filter_chains`。而在 `ring_needs_reconfigure` 分支(明确因 Fs 变化触发,`app.rs:618`)里,重建了固定 LFP/AP 链(`app.rs:624-625`)却**遗漏了用户链**。biquad 系数是 Fs 的函数(`build_filter_chains` `app.rs:497/501/505`),Fs 变而通道数不变时旧系数继续套用。可达路径:打开另一个不同 Fs、同通道数的 `.kvraw` 回放文件(`select_source→Playback` 不清链)。
- **影响**:如 30kHz→25kHz,所有 HP/LP/notch 截止按 Fs 比例偏移——60Hz notch 落到 ~72Hz,不再除工频;显示波形与 FFT 输入被静默错滤,无任何提示。
- **建议**:在 `ring_needs_reconfigure` 分支内也调 `self.rebuild_filter_chains(sample_rate, ch_count)`,与固定链的重建保持一致。

### `M3` [scientific-accuracy] 主波形 spike 计数/阈值检测跑在渲染抽取后的 peak-hold 点上 → 不应期随缩放变化、σ 被抬高、计数随窗宽变
- [ ] **定位**：`waveform.rs:695`(在抽取点上调用)/ `:729`(σ)/ `:732`(不应期)　**验证**：CONFIRMED + PLAUSIBLE　**置信度**：high
- **问题**:`detect_neg_spikes` 作用于 `collect_channel` 返回的 `pts`——**已按 stride2 峰值保持抽取到 ≤2000 点/整窗**,并非环速率样本。三重后果:
  1. **不应期尺度错配**:`refractory = (ring_sr*0.001)≈7`(环样本单位),却拿去比 `pts` 的索引差(`i-l`,`:742`)。每个 pts 索引跨 `stride2` 个环样本;默认 5s 窗 stride2≈18 → 有效不应期 ≈17ms(而非 1ms),20s 窗 ≈70ms。窗越宽死区越长。
  2. **σ 被抬高**:σ 取自这些**逐桶取极值**的点的方差(`:728-731`),极值选择使方差系统性偏大,`-Nσ` 阈值因此比真实 N 倍噪声更深,屏上红色阈值线也比标称 Nσ 更深。
  3. **计数随窗宽变**:同一信号在不同时间刻度上报不同 spike 数——该读数看似神经生理指标,实为显示抽取的伪产物。
- **影响**:每-lane spike 计数徽标与红色阈值线不是稳定的放电度量;宽窗下 >~60Hz 的连发被死区抹掉,读数随缩放漂移且系统性欠计。仅显示,不影响录制。
- **建议**:在**固定速率、最小抽取的缓冲(或环速率样本)上**做检测,独立于渲染窗;或至少把不应期换算到 pts 的真实间距 `ms_per_ring*stride2`,并用鲁棒估计(如 median(|x|)/0.6745)估噪声。

### `M4` [scientific-accuracy] PSD 窗归一化用了相干增益而非窗功率 → Hann 恒定 +1.76dB 误差
- [ ] **定位**：`fft_panel.rs:183`　**验证**：CONFIRMED　**置信度**：high
- **问题**:`win_norm = Σw/n`(相干增益,`:161-165`),功率除以 `n*ring_sr*win_norm²`(`:183`)。正确 PSD 应除以 `Fs*Σ(w²)`(窗功率)。代码分母 = `Fs*S1²/n`,正确 = `Fs*S2`;Hann 下 `S1≈0.5n`、`S2≈0.375n`,比值 `0.25/0.375=0.667`,即算出功率**大 1.5×(+1.76dB)**。代码注释自称按『窗功率 win_norm²』归一化,但 win_norm² 是相干增益的平方,与其自述矛盾。
- **影响**:µV²/Hz 轴上每个绝对 PSD 值恒定高估 +1.76dB;噪声底、频带功率、对已知幅值正弦的标定都错这么多。相对峰形不受影响。
- **建议**:分母改用 `Σ(w²)`(累加 `win_sq += w*w`),`power /= ring_sr*win_sq`(前导 n 抵消),得 Parseval 正确的 µV²/Hz。

### `M5` [ui-behavior] "Log scale (dB)" 开关只改轴标签;线性模式把 dB 数值标成 µV²/Hz
- [ ] **定位**：`fft_panel.rs:385`　**验证**：CONFIRMED　**置信度**：high
- **问题**:`spectrum` 恒存 `db = 10*log10(power)`(`:190`),绘制恒画 dB 值。`state.log_scale` 仅在 `:385` 用于选轴标签串,**从不改变数据变换**(grep 确认)。取消勾选『Log scale (dB)』后,轴标签切成『Power (µV²/Hz)』,曲线却仍是 dB 数值(如 -60),Y 界也仍是 dB。
- **影响**:线性模式下 µV²/Hz 标签下显示的是 dB 幅值,直接误导。默认 `log_scale=true`(开箱正确),需用户主动切换才触发。
- **建议**:真正按 log_scale 分支数据变换:false 时画线性功率 `10^(db/10)` 并按线性算 Y 界;或若只打算用 dB 就移除该开关。

### `M6` [scientific-accuracy] spike 叠加的噪声估计用平均绝对值 EMA 却标为 RMS → 阈值约浅 21%
- [ ] **定位**：`spike_overlay.rs:192`　**验证**：CONFIRMED(high→medium)　**置信度**：high
- **问题**:`rms_ema` 是 `s.abs()` 的 EMA(`:192`),阈值 `-sigma*rms_ema`(`:225`)。注释与字段/UI 均称其为 RMS。但零均值高斯噪声 `E[|s|]=σ√(2/π)≈0.7979σ`(平均绝对偏差,非 RMS)。故 rms_ema 估的是 0.80σ,用户设的『4σ』实际在 `-4*0.80σ≈-3.19σ` 触发。
- **影响**:每个阈值在真实-σ 意义上比标称浅 ~20%,产生远多于 σ 控件所示的假阳性 snippet(尾概率约 20×)。仅影响显示的 spike 叠加路径(主波形计数用真实 std,不受此影响)。
- **建议**:算真 RMS(`s²` 的 EMA 再开方),或对平均绝对估计乘 1.2533;若保留则把变量/注释改名 MAD。

### `M7` [ui-behavior] snippets_for/_mut 把越界通道钳到最后一个缓冲 → 通道数减少后错通道 snippet 顶着过期 CHn 标签
- [ ] **定位**：`spike_overlay.rs:363`(及 `:369`)　**验证**：CONFIRMED　**置信度**：high
- **问题**:`snippets_for`/`_mut` 用 `ch.min(bufs.len()-1)` 把任何越界 ch 映射到最后一个缓冲(返回非空、错通道数据)。`reconfigure`(`:305`)在通道数下降时重建 bufs,但**不修剪** `multiview.rs` 中每-瓦片持久化的 `channels: Vec<SpikeChannel>`;`ch ≥ 新通道数` 的过期项无复选框可关(选择器仅遍历 `0..total_ch`),仍被渲染,而 Y 轴格式化器(`:448`)用无效 ch 号打标签 `CH{ch}`。
- **影响**:切到通道更少的源(如 demo 16ch → 更少通道硬件)后,曾选高通道的 spike 叠加瓦片会把**最后一个有效通道的 snippet 重复显示在一个或多个错误 CHn 标签下**,且无法从 UI 移除该 lane——静默的错误读数。
- **建议**:越界 ch 返回共享的 EMPTY deque 而非钳制;并/或每帧在 `draw_spike_overlay_pane` 中修剪 `ch < store.channel_count()`。

---

## 4. 🔵 Low

> 以下为影响较小或属设计取舍/文档失真/边缘可达的问题,均经验证成立。给出定位与修复要点。

- [ ] **`L1`** [correctness] `finalize_channel`(`waveform.rs:714`)按**当前窗内均值**去 DC:sweep 填充过程中均值漂移使整条已画迹逐帧上下跳;周期接近窗宽的慢信号(LFP 漂移)被按窗宽相关地压平。*PLAUSIBLE,属 Intan RHX/Open Ephys 式的显示居中取舍,建议改用固定 HP 或定长基线。*
- [ ] **`L2`** [scientific-accuracy] CAR 在 i16 整数域做(`app.rs:585`):`(data - mean) as i16` 饱和转型,大共模伪迹(运动/刺激)推过满量程会在 biquad 前被硬削。*边缘、仅主显示波形(AP/spike 带 CAR 关闭)。建议整块在 f64 工作缓冲做 CAR+biquad,末尾一次量化。*
- [ ] **`L3`** [correctness] HP 无 Nyquist 上限守卫而 LP 有(`app.rs:496` vs `:500`):HP 截止 ≥Fs/2 时 RBJ 设计退化(w0=π 时 b 系数全 0,信号被清零)。当前 30kHz 下 HP 上限 10kHz 达不到,仅低速率回放可触发。建议对称钳制 HP 到 (0,Fs/2) 并给 UI 反馈。
- [ ] **`L4`** [scientific-accuracy] FFT 时间平滑 EMA 跑在 **dB(对数)域**(`fft_panel.rs:74`),得功率的几何均值(≤算术均值),默认开启,系统性略压低功率。建议在线性功率上做 EMA 再转 dB。
- [ ] **`L5`** [scientific-accuracy] 主波形检测器(真 std,peak-hold 点,Fs/4)与 spike 叠加检测器(平均绝对 EMA,全速率)统计量与速率都不同,同通道/同 σ 下计数与阈值线互相不吻合(`waveform.rs:729` vs `spike_overlay.rs:192`)。建议统一一套检测定义与鲁棒估计。
- [ ] **`L6`** [scientific-accuracy] spike 叠加 `rms_ema` 初值 0.01(`spike_overlay.rs:176`)远高于真实 AP 噪声,系数 0.0005 使 τ≈67ms,启动/每次 reconfigure 后 ~150–200ms 阈值过大而漏检小 spike。建议从首块 `mean(|s|)` 惰性播种或前 N 样本用自适应系数。
- [ ] **`L7`** [scientific-accuracy] LFP/AP 带瓦片沿用主 FILTERS 的 spike 阈值开关(`multiview.rs:568`):对主波形开启 spike 阈值时,LFP(LP 250Hz)瓦片上也画红阈值线+非零 spike 计数——在物理上不含 spike 的带上误导。建议给 LFP 传 `spike_threshold_enabled=false`。
- [ ] **`L8`** [scientific-accuracy] 带瓦片共享主 `amp_scale`/`time_window`(`multiview.rs:562`,仅临时覆盖 visible_channels):LFP(~百µV–mV)与 AP spike(~几十µV)无法各自定标,调一个会压平另一个。建议把 amp/time 索引存入 `TileKind::LfpView/ApView` 变体。
- [ ] **`L9`** [ui-behavior] 主波形标签页标题的通道范围取自 `MainWaveform.visible_count` 字段(`multiview.rs:259`),但渲染用 `self.display.visible_channels` 且该字段从不更新——改通道数后标题仍显示旧范围(如实画 32ch 却标 `CH0–16`)。纯标题。建议标题也从 `display.visible_channels` 取。
- [ ] **`L10`** [ui-behavior] 运行中新增 LFP/AP/spike 瓦片时,其带环仅在瓦片打开后才被喂(`app.rs:655`),首个满窗内 add-time 左侧空白,易被误读为掉数。建议 add 时从 block_history 回填,或对 `ring t0 > sweep_left` 显式画『buffering』提示。
- [ ] **`L11`** [efficiency] `filter_block_with_chains` 每次两次克隆样本缓冲(`app.rs:573` `data.clone()` + `:599` `..block.clone()` 再丢弃其 data)。ingest 每块最多调 3 次。建议 `let mut out = block.clone(); 原地改 out.data`。
- [ ] **`L12`** [efficiency] FFT 瓦片开启时每帧无条件全量重算(`app.rs:1590` → O(N log N) + 新 Vec 分配),即使环未推进/已暂停也在重算同一缓冲。建议按 `ring.latest_time_ms()/len` 进展门控。
- [ ] **`L13`** [efficiency] 头注释声称零参考线用 painter 绘制『无 Line 分配』(`waveform.rs:15`、`:260-262`),实际零线/阈值线/整秒线均按 `Line::new(PlotPoints::from(vec![...]))` 每通道每帧分配(`:299-312`)。建议按注释改用 `painter.line_segment` 屏幕空间绘制,或订正注释。
- [ ] **`L14`** [state-correctness] `config_persist.apply_to` 不钳制 `visible_channels`(`:302`,与相邻已钳的索引不同):配置里 `0` 会使主波形走 `draw_empty_state` 空白直到用户拖动滑块。仅手改/损坏配置可达。建议 `.max(1)`。
- [ ] **`L15`** [ui-behavior] 每通道 enable 列表按 `visible_channels` 而非 `ch_count` 呈现(`channel_select.rs:241`):band 视图可滚到 `≥visible_channels` 的高通道并强制显示(`is_channel_enabled` 越界默认 true),用户无法隐藏它们直到调高 visible。建议列表按 `ch_count` 生成。

---

## 5. ⚪ Info

- [ ] **`I1`** [ui-behavior] `refilter_history`(`app.rs:535`)用零状态新链重滤,历史头部含 HP/notch 建立瞬变;仅当可视窗跨到已存历史最左端时可见,衰减仅数十样本。可接受/文档化。
- [ ] **`I2`** [correctness] `spike_overlay::process_block`(`:344`)按 `s*ch+c` 索引无长度校验,依赖 SampleBlock 交织不变量;同块在上游 `filter_block_with_chains` 已先按同式索引,故非独有风险。建议顶部加 `if block.data.len() < spc*ch { return; }` 防御。
- [ ] **`I3`** [scientific-accuracy] `collect_channel`(`disp_ring.rs:301`)把桶内极值样本标在**桶首槽时间**而非极值自身时间(`collect_channel_minmax` 才用极值时间)。x 位偏差 ≤1 个渲染桶(~1 像素),且与其上方防抖对齐逻辑一致,属取舍。
- [ ] **`I4`** [state-correctness] `config_persist.apply_to` 不钳制 `channels_per_group`(`:316`):`0` 时 `channel_color` 走 else 返回默认蓝,组着色静默失效(不 panic)。仅手改配置可达。建议 `.max(1)`。

---

## 6. 建议处理顺序

1. **S1 + M4 + M5**(FFT 三条):若 FFT 面板要作为定量频谱质检,须换线性抽取源、修 PSD 归一化、修 log 开关——否则建议明确降级为『包络谱』并去掉 µV²/Hz 标定宣称。
2. **M3 + M6 + L5**(spike 检测一致性):统一在固定速率数据上、用鲁棒估计做检测,消除随缩放/统计量漂移。
3. **M2**(Fs 变化系数陈旧)、**M1**(滤波切换丢历史):影响多文件回放与暂停浏览的正确性。
4. **M7 / L14 / L15**:边界与状态健壮性(错通道读数、空白显示)。
5. 其余 Low/Info 按性能与文档卫生收尾。
