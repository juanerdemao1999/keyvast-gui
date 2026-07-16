# 21 · 操作台 UI/UX 审查与优化

> 审查日期：2026-07-08　|　分支：`fix/rhd-miso-delay-chipid`　|　范围：kv-gui 操作台(egui 控制面板)
>
> 本轮聚焦**操作员控制台的 UI/UX**——视觉设计、信息架构、控件工效、破坏性操作护栏、告警可见性、无障碍/可读性、设计系统一致性、与专业采集软件(Intan RHX / Open Ephys / SpikeGLX)的对齐。这是与 [19 号(整仓代码审查)](19-review-findings.md)、[20 号(信号展示专项)](20-gui-signal-display-review.md)**正交的新维度**;信号处理/DSP/环缓冲/recorder 的正确性问题不在本轮重复。
>
> 方法:8 维度并行深审(layout-ia / visual-theme / interaction / destructive-safety / status-error / accessibility / consistency-idiom / pro-alignment)→ 每条发现由独立『对抗式验证者』对照真实源码核实 → 去重综合。**原始 45 条经验证 44 CONFIRMED / 1 PLAUSIBLE / 仅 1 条驳回,去重合并为 34 条**(15 P1 / 17 P2 / 2 P3,无 P0)。叙述为中文,技术标识符/标签保留英文。

## 0. 客观验证基线(本次修复后,本机 Windows 实跑)

| 检查 | 结果 |
|---|---|
| `cargo build -p kv-gui` | ✅ 通过 |
| `cargo clippy -p kv-gui --all-targets -- -D warnings` | ✅ 干净(含测试代码) |
| `cargo fmt -p kv-gui -- --check` | ✅ 干净 |
| `cargo test -p kv-gui` | ✅ **64 测试全过**(新增时间戳/会话目录唯一性单测 + `DEFAULT_TIME_WINDOW_IDX` 断言) |

## 1. 总体评价

KeyVast 操作台功能覆盖完整、有明显的专业底子:顶部 transport 区、Source 分段控件、可停靠的三段式左侧控制面板(ACQUIRE / DISPLAY / TOOLS)、常驻底部状态栏、中央波形/多视图区,信息架构对齐了主流电生理采集软件的心智模型。`theme.rs` 已建立颜色常量、字号阶梯、圆角、transport/状态 helper——团队**意图**做一套单一真源的设计系统。

但审查暴露四条贯穿性弱点主题:

1. **破坏性操作缺乏护栏**——停止/切换 Source/关窗/裸按键都会立即 finalize 正在进行的录制且无确认,固定文件名 `recording.kvraw` 静默覆盖上次数据(最高危,直接导致不可复现的科研数据丢失)。
2. **关键告警可见性倒挂**——最能预警数据丢失的缓冲/磁盘/丢块指标被埋在 ACQUIRE tab 或角落计数器,而操作员录制时几乎总停留在别的 tab;安全关键的 REC/LIVE 状态 pill 在不换行工具栏里是最先被裁切的元素。
3. **设计系统"名存实亡"**——`theme.rs` 声称的字号真源被约 90 处硬编码 `.size()` 绕过,FONT_DISPLAY/MICRO 成死常量,16/12 幻数散落(含状态 pill),圆角 3/4/5px 无名并存。
4. **同一动作/状态表达不一致**——录制状态在四个界面用四套词与大小写,transport 控件在工具栏与侧栏以两种交互模型重复,Record 按钮把"ARMED"状态当动作标签。

**本次已落地 Batch 1(安全、高价值、低成本的样式/常量/标签级改动)与 Batch 2 中 GUI 内可完成的数据安全护栏**;其余需引入新 Modal 状态、跨 crate 改动或属功能增强的项已逐条记录待排期。

## 2. 优先级总表(P1 → P3)

> 状态图例:✅ 本次已落地　| 🟡 部分落地(核心兜底已做,完整版待排期)　| ⏸ 暂缓(记录在案)

> 更新(2026-07-08,第二批):数据安全与告警可见性——**C2 / C10 / C21 / C22 → ✅,C1 主体完成**。详见第 6 节。
> 更新(2026-07-08,第三批):数据安全兜底 + 告警 + 一致性——**C1 / C4 / C9 / C12 / C17 → ✅**(C1 补齐 recorder Drop 兜底)。详见第 7 节。
> 更新(2026-07-08,第四批):信息架构——**C7 → ✅,C23 → 🟡**(IMPEDANCE 前移 + TOOLS 默认展开;CONFIG 独立 Settings tab 暂缓)。详见第 8 节。

| 状态 | 编号 | 维度 | 一句话问题 | 定位 (file:line) | 工作量 |
|---|---|---|---|---|---|
| ✅ | C1 | destructive-safety | Stop/Source切换/退出/裸键无确认即 finalize 录制 | app.rs:934,1667;kv-recorder/lib.rs | M |
| ✅ | C2 | destructive-safety | 固定名 recording.kvraw 静默覆盖上次录制 | kv-recorder/lib.rs:651 | M |
| 🟡 | C3 | pro-alignment | 手动录制三次点击 + "ARMED"按钮标状态非动作 | app.rs:992,1897 | M |
| ✅ | C4 | consistency-idiom | 同一录制/采集状态四界面用不同词与大小写 | app.rs:2056;panels.rs:793 | M |
| ✅ | C5 | interaction | 禁用 transport 按钮仍显示可点手型+动作 tooltip | theme.rs:473 | S |
| 🟡 | C6 | layout-ia | 工具栏/状态栏不换行单行,窄窗裁切 REC/LIVE pill | app.rs:1808;panels.rs:1020;main.rs | M |
| ✅ | C7 | layout-ia | 录制通道选择被困在 DISPLAY tab | app.rs:2293 | M |
| ✅ | C8 | visual-theme | 字号阶梯形同虚设,~90 处硬编码 .size() | panels.rs;theme.rs;app.rs | M |
| ✅ | C9 | visual-theme | 竞争色源:三种蓝、两套错误红 | panels.rs:27,986 | M |
| ✅ | C10 | status-error | 缓冲/磁盘健康仅 ACQUIRE tab 可见且无主动告警 | panels.rs:946,887 | M |
| ✅ | C11 | status-error | 录制中丢块只有角落计数器,无主动告警 | app.rs:1395 | M |
| ✅ | C12 | status-error | 丢块健康用累计终身比率且粘滞,不随录制重置 | panels.rs:1128 | M |
| ✅ | C13 | accessibility | 静息/次要文字对比度仅 2.1–2.6:1,低于 WCAG AA | theme.rs:23,94 | S |
| ✅ | C14 | consistency-idiom | Transport 标签硬编码空格伪居中且宽窄不一 | panels.rs:479;app.rs:1896 | S |
| ✅ | C15 | pro-alignment | 时间窗最短 1s,无亚秒时基检视 spike | panels.rs:39 | S |
| ⏸ | C16 | layout-ia | transport 工具栏 vs 侧栏重复且交互模型分裂 | panels.rs:478,844 | M |
| ✅ | C17 | status-error | 错误呈现不一致:设备=全局横幅,录制=折叠区 | panels.rs:983;app.rs:2092 | M |
| ✅ | C18 | status-error | Toast 锚 RIGHT_TOP 遮挡状态 pill/时钟 | toast.rs:139 | S |
| ✅ | C19 | visual-theme | running-not-recording:pill 绿而时钟黄,色义冲突 | app.rs:2039 | S |
| ✅ | C20 | interaction | 启用的 Pause 填成灰色,读起来像禁用 | app.rs:1934 | S |
| ✅ | C21 | interaction | slider/DragValue 聚焦时单键快捷键静默失效 | app.rs:1490 | M |
| ✅ | C22 | destructive-safety | Config "Load" 无确认覆盖全部实时设置 | app.rs:2427;config_persist.rs | S |
| 🟡 | C23 | layout-ia | TOOLS 大杂烩,四段全默认折叠进 tab 空白 | app.rs:2327 | M |
| ✅ | C24 | layout-ia | DISPLAY 把高频 FILTERS 默认折叠 | panels.rs:689 | S |
| ✅ | C25 | accessibility | 磁盘/缓冲严重度仅色相编码,色盲无冗余提示 | panels.rs:887,947 | S |
| ⏸ | C26 | accessibility | UI scale 藏三层深且无缩放键 | config_persist.rs:379 | M |
| ✅ | C27 | visual-theme | 圆角 3/4/5px 无命名常量并存 | theme.rs:417 | S |
| ⏸ | C28 | consistency-idiom | "Demo"与"Simulator"是同一合成源两名两路 | app.rs:1982;panels.rs:396 | M |
| ⏸ | C29 | consistency-idiom | Spike 频段被叫 "Spike view"/"Spike AP"/"AP" 三名 | multiview.rs:342 | S |
| ⏸ | C30 | consistency-idiom | 📁/🔒 emoji 超出声明的 seguisym 回退集 | panels.rs:822 | S |
| ⏸ | C31 | pro-alignment | 阻抗结果只读,无 CSV 导出/坏通道排除 | impedance_panel.rs:129 | M |
| ⏸ | C32 | pro-alignment | TTL 仅电平门控,无边沿/预触发缓冲 | trigger.rs:38 | L |
| ✅ | C33 | layout-ia | UI scale 默认=最大 1.6x,Reset 也跳最大 | config_persist.rs:80 | S |
| ✅ | C34 | accessibility | 输出目录定宽截断,长路径无法确认写入目标 | panels.rs:816 | S |

**累计落地(四批合计):24 条完整(✅)+ 3 条部分(🟡),覆盖 15 个 P1 中的 13 个(其余 C3/C6 为部分)+ 全部低成本 P2/P3。** 第一批(样式/标签/护栏雏形)见第 3 节,第二批(数据安全 + 告警可见性)见第 6 节,第三批(数据安全兜底 + 告警滑窗 + 一致性)见第 7 节,第四批(信息架构)见第 8 节。

## 3. 本次已落地明细

### Batch 1 — 样式 / 常量 / 标签(纯低风险,已全部落地)

- **C13 对比度**(`theme.rs`):`TEXT_DIM` (90,90,105)→(128,128,142)、`STATUS_IDLE` (80,80,95)→(130,130,145),使最常见的次要/静息态文字于 `BG_PANEL` 上从 ~2.4:1 提升到 ≈4.6:1,达 WCAG AA。全局一处改动即传播到所有 kv_label 键、Disconnected/REC OFF/IDLE 等。
- **C8 字号真源**(`theme.rs`/`panels.rs`/`app.rs`/`config_persist.rs`):新增 `FONT_BRAND=16`、`FONT_LABEL=12`、`FONT_TRANSPORT=14`,并激活原本死掉的 `FONT_DISPLAY=15`(时钟)。把三个 GUI 文件中约 90 处硬编码 `.size(9/10/11/12/15/16)` 全部改为对应 `theme::FONT_*` 常量(**等值替换,零视觉变化**);唯一实质微调是把离经叛道的 `.size(9.5)` 归一到 `FONT_CAPTION`。此后改 `theme.rs` 的字号即可全局生效。
- **C27 圆角常量**(`theme.rs`/`app.rs`):新增 `RADIUS_WIDGET=3`/`RADIUS_BUTTON=4`/`RADIUS_CARD=5`,并接入 `section_card`、`tier_button`、两个 transport helper 与 Source 分段控件。
- **C5 禁用按钮反馈**(`theme.rs`):`transport_button`/`transport_button_sized` 在 `enabled=false` 时改用 `CursorIcon::NotAllowed`,并把动作 tooltip 标注为 "… — unavailable",消除"看着能点、点了没反应"的 affordance 错配。
- **C14 删空格**(`panels.rs`/`app.rs`):删除所有 transport 标签的硬编码首尾空格(`"  Start  "`→`"Start"` 等),对齐交给 helper 的 `min_size`+`button_padding`。
- **C3 标签部分**(`app.rs`):工具栏 Record 三态标签由 `Record/ARMED/STOP REC` 改为动作词 `Record/Start Rec/Stop Rec`——中间态标"开始写盘"这个动作而非被动状态"ARMED",与侧栏 Record 按钮及 tooltip 对齐;状态读数交给右上 pill。
- **C19 时钟色**(`app.rs`):时钟着色改为与状态 pill 同一 `match`(red=录制 / yellow=armed / **green=live** / dim=idle),消除 running 时 pill 绿而时钟黄(=ARMED 语义)的冲突。
- **C20 Pause 色**(`app.rs`):Pause 由 `TEXT_SECONDARY` 灰改 `ACCENT_BLUE`(正常动作、实色不再像禁用),Resume 用 `ACCENT_ORANGE`(标记"显示已冻结"的异常态)。
- **C15 亚秒时间窗**(`panels.rs`/`config_persist.rs`):`TIME_WINDOWS` 前置 `0.01/0.02/0.05/0.1/0.2/0.5`s,可放大到单个动作电位(`format_time_window` 已支持 <1s 渲染为 ms、`[`/`]` 自动派生边界)。新增命名常量 `DEFAULT_TIME_WINDOW_IDX=8` 供 struct 默认与 config 默认共用,保住 5s 默认不被前置项悄悄改写。
- **C24 FILTERS 默认展开**(`panels.rs`):`default_open(false)`→`true`,高频控件不再每会话多点一下。
- **C33 UI scale 归中**(`config_persist.rs`):新增 `UI_SCALE_DEFAULT=1.0`,首启默认与 Reset 都用它(此前启动即最放大 1.6x 且 Reset 也跳最大)。
- **C18 toast 下移**(`toast.rs`):锚点 y 偏移 12→48,粘滞错误卡片不再压住工具栏状态 pill/时钟。
- **C34 输出目录 hover**(`panels.rs`):窄字段加 `on_hover_text`,显示解析后的完整目标文件夹(相对路径按 cwd 展开),让操作员确认数据落点。
- **C25 色盲冗余**(`panels.rs`):磁盘余量非绿档追加 ` LOW`/` CRITICAL` 文字,缓冲占用 amber/red 两档都给文字提示(此前仅 red 有),严重度不再只靠色相。
- **C6 部分**(`main.rs`/`app.rs`):`main.rs` 加 `with_min_inner_size([1100,640])`,并把 `app.rs` 窗口恢复钳位下限从 640/480 抬到 1100/640,使窗口再不能缩到工具栏放不下右侧状态簇的宽度。
- **C22 部分**(`config_persist.rs`):"Load" 改标 `Load…` 并加破坏性 hover 警告("Replace ALL current live settings…"),Save/Reset 也补上说明 tooltip。

### Batch 2 — 数据安全护栏(GUI 内可完成部分,已落地)

- **C1 部分**(`app.rs`):
  - **录制中锁定 Source 切换**——分段控件在 `state==Recording` 时 `add_enabled_ui(false)` 灰掉并 hover "Stop recording before switching source",复用输出目录已有的录制锁定范式,堵住"误点 Source 静默终止录制"。
  - **关窗确认 Modal**——`close_requested` 且正在录制时发 `ViewportCommand::CancelClose` 并置 `pending_quit`,由新增 `draw_quit_confirm` 弹出 `Stop & Quit`/`Keep recording` 二选一;确认时先 `stop_recording()` 正常 finalize、存 config,再 `Close`。堵住"关窗即丢/写坏录制"(含 Demo 模式头未回写的风险,因为走的是正常 finalize 路径)。
- **C11 丢块主动告警**(`app.rs`):`tick_device` 检出 `dropped_blocks` 增长且正在录制时,fire 节流 toast(≤1 次/3s)"Dropped N blocks — data lost",把不可逆丢数从角落计数器提升为主动通知。

## 4. 暂缓项(记录在案,建议后续排期)

以下项因需**较大信息架构重排或属功能增强**,尚未动:

- **一致性 / 信息架构**:**C16** transport 双面统一交互模型(Disarm/Pause 键盘可达);**C6 剩余** 工具栏/底栏响应式换行或按 `available_width` 丢弃低优先字段(min 尺寸已落地);**C23 剩余** CONFIG/UI-scale 拆成独立 SETTINGS tab(IMPEDANCE 前移 + TOOLS 默认展开已落地);**C26** 加 `Ctrl+=`/`Ctrl+-`/`Ctrl+0` 缩放键并进 help;**C28/C29/C30** Demo↔Simulator、Spike view↔AP、emoji 图标命名/风格统一。
- **专业能力补齐**:**C3 流程** 手动录制一步化 / `Armed` 保留给 TTL 触发路径(C3 标签部分已落地);**C31** 阻抗 CSV 导出 + 按阈值排除坏通道并接入 channel_select;**C32** TTL 边沿触发 / 固定时长 / 预触发缓冲(L)。

## 5. 变更文件一览

| 文件 | 涉及编号 |
|---|---|
| `crates/kv-gui/src/theme.rs` | C13, C8, C27, C5, C9 |
| `crates/kv-gui/src/panels.rs` | C14, C15, C24, C25, C34, C8, C2, C10, C12, C4, C9, C17 |
| `crates/kv-gui/src/app.rs` | C3, C19, C20, C8, C27, C6, C1, C11, C2, C10, C12, C21, C22, C4, C9, C17, C7, C23 |
| `crates/kv-gui/src/config_persist.rs` | C33, C22, C15, C8 |
| `crates/kv-gui/src/playback.rs` | C23, C8 |
| `crates/kv-gui/src/toast.rs` | C18 |
| `crates/kv-gui/src/main.rs` | C6 |
| `crates/kv-recorder/src/lib.rs` | C1(Drop 兜底) |

> 编号 `C1`–`C34` 与本文件第 2 节总表一致,便于逐项追踪。原始 45 条已核验发现与 8 维度对抗验证明细见本次评审工作流记录。

## 6. 第二批落地明细(2026-07-08 续)

聚焦四大弱点主题中最关键的**破坏性操作护栏**与**告警可见性倒挂**,并顺带清掉一处快捷键 bug。全部 `cargo build / clippy --all-targets -D warnings / fmt / test`(64 过)通过。

### 数据安全(C2 / C1 / C22)

- **C2 · 每次录制写入唯一会话文件夹**(`app.rs`/`panels.rs`):recorder 始终把固定名 `recording.kvraw` 等写进给定目录,故在 GUI 侧把目标改为 `<output_dir>/<prefix>_<UTC 时间戳>`(同秒冲突再加 `_2`/`_3`…),彻底消除"第二次录制静默覆盖第一次"。新增无依赖的 `format_utc_stamp`(Howard Hinnant civil-from-days,附单测:epoch/2021/闰日)与 `resolve_session_dir`(唯一性单测)。所有录制入口(手动 toggle、TTL 触发、远程 API)统一走 `begin_recording()`,一处修复全覆盖;`RecordingSettings.active_dir` 记录解析后路径,RECORDING 面板显示 "🔒 Writing to <folder>" + hover 完整路径;toast 改为 "Recording to <folder>"。
- **C1 · 停止方向确认 + 关窗确认 + 录制中锁源**(`app.rs`):
  - 关窗:录制中 `close_requested` → `ViewportCommand::CancelClose` + `draw_quit_confirm`(Stop & Quit / Keep recording),确认时先正常 `stop_recording()` finalize 再退出。
  - 停止:新增 `PendingStop{Recording,Acquisition}` + `draw_stop_confirm`;交互式 `toggle_recording`/`toggle_acquisition`/侧栏 Stop 在会终结录制时改为弹确认,**自动化路径(TTL 触发、远程 API)不受影响仍即时停止**。未录制时停止无摩擦。
  - 锁源:录制中 Source 分段控件 `add_enabled_ui(false)` + hover。
  - 仍缺:Demo `StreamingRecorder` 的 `Drop` 兜底(见第 4 节)。
- **C22 · Config Load 二次确认**(`app.rs`/`config_persist.rs`):Load 按钮改标 `Load…` + 破坏性 hover;点击不再立即覆盖,而是 `pending_load` → `draw_load_confirm`(Load / Cancel),确认后才 `apply_loaded_config()`。

### 告警可见性(C10 / C11)

- **C10 · 缓冲/磁盘健康进常驻底栏 + 主动 toast**(`app.rs`/`panels.rs`):底栏 `draw_status_bar` 新增两个 chip——录制中 `Buf NN%`(阈值 0.40/0.75,>0.75 加 " HIGH" 文字)、常驻 `Disk NNG`(阈值 2/10 GB,加 " LOW")。磁盘余量按 ~1Hz 缓存轮询,避免每帧 syscall。新增 `update_health_alerts`:录制中缓冲/磁盘**首次**跨入 amber/red 各 fire 一次性 toast(高水位随录制结束重置),告警不再依赖操作员停在哪个 tab。
- **C11**(第一批):录制中丢块节流 toast,已见第 3 节。

### 交互 bug(C21)

- **C21 · 快捷键焦点门控**(`app.rs`):`handle_keys` 由"任何控件聚焦即禁用快捷键"改为 `ctx.wants_keyboard_input()`——只有文本输入真正吃键时才抑制,滑块/DragValue 持焦不再让 Space/R/G/P/F/`[` `]` 静默失效。

## 7. 第三批落地明细(2026-07-08 续)

补完数据安全兜底与告警,再统一一致性。全部 `cargo build / clippy --workspace --all-targets -D warnings / fmt / test`(kv-gui 64 过、kv-recorder 29 过,新增 3 个单测)通过。

### 数据安全兜底(C1 完成)

- **C1 · `StreamingRecorder` Drop 兜底**(`kv-recorder/src/lib.rs`):把 `finish()` 的头回写抽成 `&mut self` 的 `finalize_header()`(用 `BufWriter::get_mut()` 取可 seek 的 `&mut File`,不再 `into_inner()` 消费 self),并加 `finalized` 幂等标志。新增 `impl Drop`:未经 `finish()` 就被 drop 时(app 退出 / 线程 unwind)best-effort 回写头,使 `.kvraw` 落为 `json_len>0` 的合法 v2 文件而非零头坏文件;错误经 `eprintln!`(该 crate 无 log 依赖,保持精简)。新增集成测试 `dropping_without_finish_still_leaves_a_valid_readable_file`:不调 finish 直接 drop,再用 `KvrawReader::open` 验证 `format_version==2`、`channel_count`/`block_count` 正确且数据可读。至此 C1 全部收口。

### 告警(C12)

- **C12 · 丢块健康改滑窗**(`app.rs`/`panels.rs`):新增 `drop_events: VecDeque<(Instant,u64)>`,`tick_device` 检出丢块时入队;每帧 `recent_drops()` 修剪到最近 `DROP_ACTIVITY_WINDOW_SECS`(3s)并求和。`draw_status_bar` 的丢块指示改按**近况**上色而非累计终身比率:`recent_drops>0` → 红 "⚠ Dropping +N"(正在丢)、`dropped>0` 但近况为 0 → 中性 "Drops: N"(已恢复)、否则 → 绿 "Drop: 0";hover 同时给"近 3s / 累计"。操作员现在能一眼回答"我现在还在丢吗"。

### 一致性(C4 / C9 / C17)

- **C4 · 状态标签单一真源**(`panels.rs`/`app.rs`):新增 `panels::acq_label(running)`→`LIVE`/`IDLE` 与 `panels::rec_label(state)`→`REC`/`ARMED`/`OFF`。工具栏状态 pill、底栏采集/录制状态、侧栏 ACQUISITION 状态行统一改走这两个函数——消除 `ACQ`/`ACQUIRING`/`LIVE`、`REC OFF`/`OFF` 等同义异形(侧栏 RECORDING 段的 Title-Case 详情行按报告允许保留)。
- **C9 · 颜色来源收敛**(`theme.rs`/`panels.rs`):`CHANNEL_GROUP_COLORS` 改为 `theme::CHANNEL_PALETTE` 的策划子集(索引引用),分组色与全局 accent 不再是三种近似蓝;未分组默认色也改引 `CHANNEL_PALETTE[1]`。新增 `theme::ERR_BANNER_BG/ERR_BANNER_TEXT` 单一错误面色。
- **C17 · 错误横幅统一为全局顶栏**(`app.rs`/`panels.rs`):抽出 `KvApp::draw_error_banner(ctx,id,prefix,msg)->bool`,设备错误与录制错误现都渲染为同款可关闭顶部横幅(共用 `ERR_BANNER_*`);删除埋在折叠 RECORDING 段里的内联录制错误卡片,并清掉沿途 `recording_error`/`dismiss_error` 参数链(`draw_recording_section`/`draw_acquire_core` 各减两参)。

## 8. 第四批落地明细(2026-07-08 续)

聚焦信息架构:把录制关键决策和电极 QC 放到它们该在的位置。`cargo build / clippy --all-targets -D warnings / fmt / test`(64 过)通过。

- **C7 · 录制通道选择前移到 ACQUIRE**(`app.rs`):ACQUIRE 的 RECORDING 摘要由被动面包屑"…configure in DISPLAY ▸ CHANNELS"改为**可操作控件**——"Record subset only" 复选框(录制中锁定)+ "Recording N of M channels" 摘要 + 勾选后出现的 "Edit…" 小按钮,点击经 `jump_to_display` 直接切到 DISPLAY 的每通道 Rec 选择器。选录哪些通道从此是 arming 的一部分,不必跨 tab+展开折叠区;完整每通道编辑器仍留在 DISPLAY(单一真源 `channel_select` 状态,两处共享)。
- **C23 · TOOLS 重排 + 默认展开**(`app.rs`/`playback.rs`):**IMPEDANCE 前移到 ACQUIRE**(紧随 device/acquisition/recording,因为阻抗是电极/设备 QC 步骤,而非杂项工具);TOOLS 现只留 PLAYBACK / REMOTE API / CONFIG,并把残余首段 PLAYBACK 改 `default_open(true)`,进 tab 不再是一片折叠标题的空白。顺带把 `playback.rs` 漏网的 `.size(11.0)` 归一到 `FONT_HEADING`(C8 补漏)。
  - **仍缺(🟡)**:把 CONFIG/UI-scale 拆成独立 SETTINGS tab(需新增第 4 个 `SidebarTab` 变体);本轮保留在 TOOLS。
