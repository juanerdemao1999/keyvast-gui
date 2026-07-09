# 19 · 全面代码审查发现清单

> 审查日期：2026-06-24　|　分支：`gui-ux`　|　范围：整个 workspace（9 crate，约 22,000 行 Rust）
>
> 本文件是一次全面审查的**可追溯跟踪清单**。每条发现带稳定编号（如 `C1`/`H3`）、精确 `文件:行号` 定位、严重度、类别、影响、修复建议与置信度，并附勾选框便于团队逐项消项。叙述性章节为中文；High/Medium/Low/Info 的明细正文保留英文原文，以确保技术措辞精确、便于检索代码标识符。

## 0. 审查方法

两路交叉验证：

1. **客观基线**（在本机 Windows 实跑）：`cargo test` / `cargo clippy --all-targets -D warnings` / `cargo fmt --check`。本机可原生编译 GUI（CI 在 Linux 上只能跨目标 `check`，不链接、不测）。
2. **多智能体评审**：14 个子系统并行评审 → 每条发现由独立『对抗式验证者』读真实代码核实 → 完整性批判 → 综合。

原始 123 条发现，经对抗式验证 **120 条成立、3 条被驳回**，完整性批判补充 10 条。4 个 Critical 已由人工逐一复核源码确认。

| 严重度 | 数量 |
|---|---|
| 🔴 Critical | 4 |
| 🟠 High | 24 |
| 🟡 Medium | 43 |
| 🔵 Low | 52 |
| ⚪ Info | 7 |
| **合计** | **130** |

## 1. 总体评价

> KeyVast 是一个架构扎实、工程认真的原型，但**尚未达到可用于活体在体记录的生产标准**。crate 依赖图干净无环、kv-types 是规范的契约叶子层、fanout 缓冲正确隔离了 recorder 与 preview 消费者、RHD 硬件 bring-up（MISO 扫描、半量程门控、PLL 处理）体现真实领域功底、KVRAW v2 格式设计合理并含崩溃检测。**但系统存在多个直接导致实时采集平台两个最坏结局——采集卡顿与静默丢数据——的缺陷**：磁盘 I/O 在持有共享 pipeline 互斥锁时进行（拖停生产者）；缓冲溢出丢块却从不发出代码库自己定义的 AcquisitionEvent::BufferOverflow；RHD 导出对每段默认配置录音静默截断、且任何真实会话都会 OOM；integrity 在 packet_id 回绕时崩溃；FFT 面板显示科学上错误的频率；远程 API 绑定 0.0.0.0 且无认证。**这些都不是冷僻边界情况——数条在默认配置下即触发**。在能托付不可复现的实验数据之前，代码库需要对采集热路径、导出路径、错误传播、FFI 与测试覆盖做一次集中加固。

### 客观验证基线（人工实跑）

| 检查 | 结果 |
|---|---|
| `cargo test`（非 GUI 工作区） | ✅ **103 测试全过**；但几乎全是 `tests/` 集成测试，`lib.rs` 内联单测极少，**kv-gui 无集成测试** |
| `cargo clippy` 生产代码（含 GUI，`-D warnings`） | ✅ 干净 |
| `cargo clippy --all-targets` | 🔴 **kv-gui 测试代码 4 个错误**（`channel_select.rs` ×3 `field_reassign_with_default`；`config_persist.rs:447` 将 `3.14` 当 PI）——**CI 抓不到** |
| `cargo fmt --check` | 🔴 **脏**（分支未按 rustfmt 格式化，主要在 `kv-cli/src/lib.rs`） |
| CI 触发 | 🔴 仅 `workflow_dispatch` 手动；push/PR **从不自动运行**；test job `--exclude kv-gui`；GUI clippy 不带 `--all-targets` |

## 2. 🔴 Critical（4）

> 以下 4 条均直接导致实时采集系统两个最坏结局——**采集卡顿**或**静默丢数据**，且数条在默认配置下即触发。均经人工复核源码确认（正文中文）。

#### `C1` [concurrency] Disk I/O inside drain_streaming holds the shared Mutex, blocking the producer thread
- [ ] **定位**：`crates/kv-core/src/pipeline.rs:319-324`　**置信度**：high　**子系统**：core-pipeline
- **问题**：在 run_streaming_pipeline 的消费循环中，drain_streaming 在仍持有 SharedState 的 MutexGuard 时调用 recorder.write_block() 与 integrity.push()——二者都是可能阻塞不确定时长的同步操作（磁盘写延迟、OS 调度）。其间生产者线程无法获得锁来推入新块。30kHz×64ch 下每约 0.033ms 到达一个块，即便 5ms 磁盘抖动也会填满容量 128 的环并开始静默丢块。直接违反『热路径禁止阻塞采集线程』与铁律 4（recorder 失败不得损坏录制）。
- **影响**：任何磁盘抖动（OS 写回刷盘、杀软扫描、NVMe 休眠）都会阻塞生产者线程、填满有界 recorder 环、导致静默丢块。dropped_blocks 计数虽增加但采集线程不感知、不抛任何错误——最坏情况是录制中出现连续丢包且无任何错误路径。
- **建议**：持锁期间只把可用的 Arc<SampleBlock> 收集进局部 Vec，随即释放锁，再在临界区外执行写盘；或将 StreamingRecorder 移到独立写线程并配自有有界通道，使磁盘 I/O 与共享锁彻底解耦。

#### `C2` [data-integrity] RHD export silently discards trailing data — always lossy with default 64-sample blocks
- [ ] **定位**：`crates/kv-recorder/src/export_formats.rs:109-133`　**置信度**：high　**子系统**：recorder
- **问题**：写循环条件 while offset + RHD_SAMPLES_PER_BLOCK <= total_samples 会丢弃所有凑不满 128 帧整块的尾部帧，无错误、无警告、返回值也不提示。默认配置 64 样本/包（DEFAULT_SAMPLES_PER_PACKET=64），3 块导出共 192 帧：只写出一个完整 RHD 块（0..128），其余 64 帧（33% 数据）被静默丢弃。现有测试用 make_test_blocks(4,128,2) 恰为 128 的整数倍，因此通过却掩盖了 bug。
- **影响**：任何帧数非 128 整数倍的 RHD 导出都会丢数据且不通知调用方；默认 64 样本/块配置下每次导出都丢最后一批。导出的 RHD 文件与源 KVRAW 不一致，破坏下游分析。
- **建议**：循环后若 offset < total_samples，发出 RecorderError::Io（或新增 PartialBlock 变体），对尾帧补零填满最后一个 RHD 块或直接报错；并增加帧数非 128 整数倍的测试。

#### `C3` [bug] FFT frequency axis and PSD normalization use wrong sample rate (full Fs instead of decimated Fs)
- [ ] **定位**：`crates/kv-gui/src/fft_panel.rs:60-91`　**置信度**：high　**子系统**：gui-render
- **问题**：last_n_samples() 返回显示环中的样本，而该环只存 1/RING_DWNSP（当前 1/4）。30kHz 硬件速率下环实际为 7500Hz。但 compute_spectrum() 在 draw_fft_section() 中以 sr=block.sample_rate（30000Hz）调用，而非 ring.sample_rate/RING_DWNSP（7500Hz）。后果：(1) 每个频点标高 4 倍——100Hz 神经信号显示为 400Hz，显示 Nyquist 为 15000Hz 而非 3750Hz；(2) PSD 分母 (n*sample_rate) 大 4 倍，功率低约 6dB；(3) 50/60Hz 工频标记落在错误位置。
- **影响**：全部频谱信息错误。操作者排查工频污染或核对已知刺激频率时会得出错误结论——该面板当前形态在科学上无效。
- **建议**：将 ring.sample_rate / ring.dwnsp 作为有效采样率传入 compute_spectrum，或在 DisplayRing 上暴露 effective_sample_rate()。

#### `C4` [concurrency] RemoteApiHandle::stop() deadlocks GUI shutdown when a non-default port is used
- [ ] **定位**：`crates/kv-gui/src/remote_api.rs:148-154`　**置信度**：medium　**子系统**：completeness-critic
- **问题**：stop() 通过连接 127.0.0.1:DEFAULT_PORT(7310) 来解阻塞 accept() 循环。若服务器启动在其他端口（用户可经配置 remote_port 修改），connect() 连错端口、静默失败（结果被 let _ = 丢弃），accept() 循环永不解阻塞，随后 t.join() 永久阻塞，在关窗或任何 drop 该 handle 的路径上挂起整个 GUI 进程。这是死锁，而非单纯的停止竞态。
- **影响**：只要配置了非默认远程 API 端口，GUI 关闭时即无限挂起，Windows 上须从任务管理器强杀；此时进行中的录制不会被正常收尾。
- **建议**：在 RemoteApiHandle 中保存实际绑定端口并在此使用：let _ = TcpStream::connect(format!("127.0.0.1:{}", self.bound_port));，或改用 SO_REUSEADDR 唤醒令牌 / 关闭管道替代自连接。

## 3. 交叉主题（综合归纳）

### 1. 采集热路径的实时安全性
_涉及：crates/kv-core/src/pipeline.rs, crates/kv-gui/src/live_pipeline.rs, crates/kv-buffer/src/lib.rs_

多处缺陷违反『热路径不得阻塞采集线程或做每样本分配』的铁律。最严重者是 drain_streaming 持有共享 Mutex 时做磁盘 I/O（pipeline.rs:319-324），任何写回刷盘/杀软扫描/NVMe 休眠都会阻塞生产者并填满有界 recorder 环。同样的『持锁拷贝』模式重现于 run_threaded_pipeline（pipeline.rs:179-183、256-259），FanoutBlockBuffer::push 在持锁时分配 Arc（live_pipeline.rs:256-259）。每块多余克隆（live_pipeline.rs:247-258、drain_consumer）增加约 3.75 MB/s 可避免的分配。修复模式一致：锁内只取 Arc 指针，释放锁后再做 I/O 与克隆，或把 recorder 移到独立写线程。

### 2. 静默数据丢失与未上报失败（违反『禁止静默错误』）
_涉及：crates/kv-buffer/src/lib.rs, crates/kv-rhd/src/backend.rs, crates/kv-recorder/src/lib.rs, crates/kv-cli/src/lib.rs, crates/kv-integrity/src/lib.rs_

代码反复丢数据或在坏数据上继续而不报错。BufferOverflow 丢块但从不发 kv-buffer 自己定义的 AcquisitionEvent::BufferOverflow（lib.rs:29-37）。PLL 锁定超时被吞致采样率错误（backend.rs:1076-1094）。flush_fifo 吞掉 wire-in/pipe 错误（backend.rs:1023-1062）。set_sample_rate_30khz 丢弃接受位 bool（backend.rs:414-417）。KvrawReader v1 回退伪造 sample_rate=30000/channels=64 而非报错（lib.rs:1042-1057）。CLI 在给 --preset 时静默丢弃 --duration（lib.rs:1105-1110），并写入硬编码零事件时间戳（lib.rs:775-840）。integrity 把每个丢包额外误计为时间戳不连续（lib.rs:136-153）。修复一致：传播 Result/错误或触发已有事件，而非丢弃信号。

### 3. 导出与大文件内存安全（OOM）
_涉及：crates/kv-recorder/src/export_formats.rs, crates/kv-recorder/src/lib.rs, crates/kv-gui/src/app.rs_

整个导出/回放路径假设录音能放进内存。export_intan_rhd 在写出前缓冲整段录音（export_formats.rs:93-104）——10 分钟约需 7GB——并额外丢弃所有非 128 整数倍的尾块（即默认配置的每段录音，export_formats.rs:109-133）。GUI export_kvraw 虽看似分块实则把整段录音累积为 Vec<SampleBlock>（app.rs:2354-2383），有 OOM 致采集中进程被杀的风险。KvrawReader::read_frames 使用未检查的 usize 算术（lib.rs:1107-1110）。均需改为流式/分块并使用 checked 算术，把内存上限压到 O(channels*block)。

### 4. 硬件无关性被破坏（设备常量泄漏）
_涉及：crates/kv-gui/src/fft_panel.rs, crates/kv-cli/src/lib.rs, crates/kv-rhd/src/impedance.rs, crates/kv-rhd/src/protocol.rs_

铁律 1 禁止在上层硬编码 ADC 因子、寄存器映射、bitfile、通道映射，但 RHD 专属常量散落各处。0.195uV/count 与满量程换算硬编码在 GUI FFT 面板（fft_panel.rs:72）。CLI 经 env!(CARGO_MANIFEST_DIR) 硬编码 FPGA bitfile 名、电缆长 0.914m、device-id 字符串（lib.rs:1194-1201、546、591）。阻抗计算硬编码 1.225V DAC 基准与 uV/count（impedance.rs:146-158）。protocol 到通用配置桥硬编码 TTL 线数与 USB 传输（protocol.rs:112-121）。各项应改为引用由产出后端携带在 SampleBlock/DeviceStatus 上的单一命名常量，或运行时可配置。

### 5. FFI 与 unsafe 代码安全卫生
_涉及：crates/kv-rhd/src/frontpanel.rs, crates/kv-rhd/src/parser.rs, crates/kv-core/src/process_metrics.rs, crates/kv-gui/src/diskspace.rs_

全库最高风险代码——Opal Kelly FrontPanel FFI——所有 unsafe 调用点零 SAFETY 注释（frontpanel.rs:50-291），使日后重构 Arc<FrontPanelApi> 生命周期或句柄别名极易引入 UAF 且无据可查。process_metrics.rs(115-148，对 Windows 结构体用 std::mem::zeroed) 与 diskspace.rs(38-46) 同样缺失。一个 c_long 缓冲长度强转在 Windows 上可能静默截断且无 debug_assert（frontpanel.rs:43、219）。parser 读 helper 越界时 panic 而非返回 RhythmParseError（parser.rs:193-222）。均可通过补 SAFETY 注释（记录生命周期/非空/独占所有权/NUL 终止不变量）、加 debug 断言、把读 helper 改为返回 Option/Result 解决。

### 6. GUI 边界不得拖垮采集（铁律 4）
_涉及：crates/kv-gui/src/config_persist.rs, crates/kv-gui/src/spike_overlay.rs, crates/kv-gui/src/app.rs, crates/kv-gui/src/remote_api.rs_

数处 GUI 侧 panic/挂起会违反『GUI 失败不得停止采集或阻塞收尾』。RemoteApiHandle::stop() 在非默认端口下死锁 GUI 关闭（remote_api.rs:148-154）。live_pipeline 取用 unwrap（app.rs:1219）。notch_idx、channel_spacing 从配置加载未做边界检查会 panic（config_persist.rs:192）。spike 的 snippets_for() 未防零通道。修复：把 unwrap 改 let-else、加边界检查、隔离 GUI 线程 panic 使其不影响采集/录制收尾。

### 7. 实时视图的科学/显示正确性
_涉及：crates/kv-gui/src/fft_panel.rs, crates/kv-gui/src/waveform.rs, crates/kv-gui/src/disp_ring.rs, crates/kv-gui/src/spike_overlay.rs_

实时视图是操作者唯一的在线信号质量检查手段，但多处数值 bug 使其失真。FFT 频率/PSD 用错采样率（fft_panel.rs，见 C3）；spike 不应期用整满采样率索引抽取后的槽数组，致不应期长 4 倍（waveform.rs:684-693）；hover 幅值读数遗漏增益项，致非默认 amp_scale 下读数错约 12.8 倍（waveform.rs:457-460）；PSD 缺 Hann 窗归一化致功率未标定（fft_panel.rs:78-88）。修复后频率、功率、spike 计数、幅值才正确。

### 8. 高风险代码的测试覆盖
_涉及：crates/kv-rhd/src/backend.rs, crates/kv-rhd/src/protocol.rs, crates/kv-integrity/tests/, crates/kv-recorder/tests/, crates/kv-gui/, .github/workflows/ci.yml_

决定后续采集成败的硬件 bring-up 路径几乎无测试：RHD probe/帧分析 helper（backend.rs:1461-1675）、RhdChipType::from_register63 芯片分发（错了会静默丢一半通道）、MISO 扫描、寄存器打包全无单测；KvrawReader 错误路径、StreamingRecorder 一致性检查、PacketIdWentBackwards 均未覆盖；kv-gui 完全无集成测试且被 CI 排除。应为这些最高风险且当前未测的代码补单测，并启用 Windows CI job 编译并测试 kv-gui。

### 9. 构建/CI 卫生与仓库整洁
_涉及：.github/workflows/ci.yml, .gitignore, .cargo/config.toml, third_party/opalkelly/, docs/, compare_kvraw_vs_oe.py_

CI 仅 workflow_dispatch（从不自动运行）却被文档宣称每次 PR/push 运行；CI 从不真正编译/链接或测试 kv-gui；cargo fmt --check 当前为脏。仓库根存在未被 .gitignore 覆盖的 2.6MB 专有 .bit、诊断 .py 脚本、captures/、__pycache__/、*.png、kvlog.txt，一次 git add -A 即会误提交专有二进制与含硬编码本机路径的脚本。第三方 okFrontPanel.dll 无 license/出处/校验和。应恢复自动 CI 触发并覆盖全部 crate（含 kv-gui）、全量 fmt、收紧 .gitignore、给 DLL 加 NOTICE。

### 10. 架构/可维护性债务
_涉及：crates/kv-gui/src/app.rs, crates/kv-rhd/src/backend.rs, crates/kv-cli/src/lib.rs, crates/kv-gui/src/panels.rs, crates/kv-simulator/src/lib.rs, docs/03-architecture.md, Cargo.toml_

四个文件远超 800 行上限（app.rs 2403、backend.rs 1676、kv-cli/lib.rs 1288、panels.rs 1201）。帧布局算术在 backend.rs 与 parser 间重复 5+ 处并已出现分歧，易静默错位。文档宣称的 DeviceBackend trait 实际不存在，真正的抽象是更薄的 AcquisitionSource。应拆分超限文件、抽取单一 FrameLayout helper、引入真正的 DeviceBackend trait（或在文档中明确 AcquisitionSource 即契约）并让 SimulatorBackend 实现它。

## 4. 🟠 High（24）
> 明细正文保留英文原文以保技术精确；分类、定位、字段标签为中文。

#### `H1` [project-rule] RHD ADC conversion factor 0.195 microvolts per count hardcoded in GUI
- [ ] **定位**：`crates/kv-gui/src/fft_panel.rs:72`　**置信度**：high　**子系统**：architecture
- **问题**：The Intan RHD amplifier conversion factor 0.195 microvolts per count is hardcoded as a bare literal in the GUI FFT panel and again in waveform.rs as RHD_FULL_SCALE_UV. Rule 1 forbids hardcoding ADC conversion factors in upper layers before hardware confirmation, and kv-rhd already owns this as RHD_AMPLIFIER_MICROVOLTS_PER_COUNT. The GUI should depend only on the SampleBlock contract which carries no microvolt scale.
- **影响**：Display is silently coupled to one headstage gain; other backends produce incorrect readouts. Violates hardware-independence and no-magic-values rules.
- **建议**：Carry a microvolts-per-count scale on SampleBlock or DeviceStatus populated by the producing backend; until then reference a single named constant instead of duplicating 0.195.

#### `H2` [data-integrity] BufferOverflow AcquisitionEvent is defined but never emitted on block drop
- [ ] **定位**：`crates/kv-buffer/src/lib.rs:29-37`　**置信度**：high　**子系统**：types-buffer
- **问题**：When ConsumerQueue::push() or BlockBuffer::push() silently discards the oldest block because the queue is full, it only increments dropped_blocks. The AcquisitionEvent::BufferOverflow variant declared in kv-types/src/lib.rs (line 210-213) is never constructed or emitted anywhere in the codebase — confirmed by grep showing it appears only in the type definition and in the recorder's CSV serializer. Project Rule 3 (avoid SILENT error handling; record buffer overflows) is violated: a consumer watching for this event will never receive it, and the dropped_blocks counter is only visible by polling consumer_status().
- **影响**：Operator and automated monitors lose visibility into real-time data loss. A slow recorder consumer can silently drop acquisition blocks with no logged event, no AcquisitionEvent, and no notification outside of a manual status poll. This is exactly what the rule prohibits.
- **建议**：The push() methods should return a boolean or a dedicated OverflowInfo value indicating whether a block was dropped, so callers (e.g. FanoutBlockBuffer::push or the pipeline layer) can emit AcquisitionEvent::BufferOverflow with the correct dropped_blocks count and buffer_occupancy. Alternatively, accept an optional event sink callback or channel in push() to fire the event directly. The FanoutBufferStatus::pushed_blocks already exists; add a top-level dropped_blocks_total field and surface it to the pipeline, which can then emit the event.

#### `H3` [memory-safety] All unsafe FFI call sites lack SAFETY comments · 初判 critical
- [ ] **定位**：`crates/kv-rhd/src/frontpanel.rs:50-291`　**置信度**：high　**子系统**：rhd-hardware
- **问题**：Every unsafe block that calls into okFrontPanel.dll — library loading (line 50), all function pointer dereferences (lines 98, 119, 122, 154, 157, 174, 186, 190, 194, 204, 214, 235, 248, 253, 261, 280, 285) — is missing a // SAFETY: comment documenting the invariants that make each call sound. The project rules and the Rust security guidelines both require this. The FFI calls pass raw C pointers, write into caller-supplied buffers, and rely on correct lifetime ordering of the Library, the handle, and the function-pointer table; none of these invariants are stated.
- **影响**：Any future refactor of the Arc<FrontPanelApi> lifetime, handle aliasing, or DLL unload ordering is done without documented invariants, making it easy to introduce use-after-free or dangling-pointer UB. Code reviewers and auditors cannot evaluate safety without these comments.
- **建议**：Add a // SAFETY: comment before every unsafe block. At minimum state: (1) the Library is kept alive for at least as long as all derived function pointers via Arc<FrontPanelApi>._library; (2) handle is non-null (checked after construct()); (3) the handle remains exclusively owned by one FrontPanelDevice (no aliasing); (4) buffers passed to read_from_block_pipe_out are valid for the declared length; (5) CStrings are NUL-terminated and ASCII.

#### `H4` [error-handling] wait_for_dcm_done and wait_for_data_clock_locked silently swallow timeout — PLL failure goes undetected
- [ ] **定位**：`crates/kv-rhd/src/backend.rs:1076-1094`　**置信度**：high　**子系统**：rhd-hardware
- **问题**：Both polling helpers spin up to 100 ms and then return silently with no error if the FPGA PLL never locks. set_sample_rate() calls wait_for_dcm_done() and wait_for_data_clock_locked() and then continues programming the FPGA as if the clock is good. If the DCM or PLL lock never asserts (bad bitfile, marginal power supply), all subsequent register writes and data reads are performed with an incorrect sample clock, producing corrupted ADC data that will look valid from the parser's perspective.
- **影响**：Acquisition proceeds silently on data sampled at the wrong rate. There is no downstream detection of a PLL lock failure — the recording is silently corrupt.
- **建议**：Change both functions to return Result<(), RhdReadError> and return Err(RhdReadError::ClockNotLocked) (add a new variant) when the loop exhausts. Propagate the error through set_sample_rate and up to configure, so initialization is aborted when the FPGA clock does not lock.

#### `H5` [error-handling] Silenced errors in flush_fifo prevent detection of wire-in or pipe-read failures
- [ ] **定位**：`crates/kv-rhd/src/backend.rs:1023-1062`　**置信度**：high　**子系统**：rhd-hardware
- **问题**：flush_fifo() uses `let _ = self.device.set_wire_in_value(...)` and `let _ = self.device.read_from_block_pipe_out(...)` four times. These are the only two places in the entire file where FrontPanel errors are deliberately suppressed. The project rule 'AVOID SILENT error handling' is explicitly violated. The flush is called both during board configuration and before each impedance measurement channel — a silent failure leaves the FIFO with stale data, causing the parser to read interleaved acquisition state.
- **影响**：A hardware transient during flush silently fails; the FIFO is not cleared; subsequent reads consume stale or garbled frames. Impedance measurements and the post-configuration flush both depend on this working correctly.
- **建议**：flush_fifo should return Result<(), RhdReadError>. Log but tolerate individual read errors (a short read during flush is expected), but surface wire-in failures. At minimum replace `let _ =` with `.map_err(|e| log::warn!("flush_fifo: {e}"))` and explain why the error is non-fatal.

#### `H6` [bug] Filler-word formula is wrong for single-stream mode and inconsistent with Rhythm FPGA spec
- [ ] **定位**：`crates/kv-rhd/src/protocol.rs:184-189`　**置信度**：high　**子系统**：rhd-parsing
- **问题**：The Rhythm FPGA always pads the per-sample amplifier section to a 4-stream word boundary. The correct filler count is `(4 - streams % 4) % 4`. The code uses `streams % 4` instead, which gives 1 filler word for streams=1 (correct value: 3) and 3 for streams=3 (correct: 1). For the current hardware cap of streams=2 both formulas coincidentally agree (2 filler words either way), so this does not trigger a real regression today. However: (a) MAX_SUPPORTED_STREAMS=2 is a temporary cap, not a permanent architectural constraint; (b) the CLI smoke test exercises the streams=1 path on real bytes generated by the same wrong formula, so the test cannot detect the mismatch against hardware; (c) the comment in the parser says 'align each frame to a 4-stream boundary', which contradicts the formula. If a single-stream headstage is ever connected to real hardware the parsed frame offsets will be 4 bytes off per sample, producing garbled waveform data and false magic-mismatch errors on every subsequent sample.
- **影响**：Silent data corruption or cascade BadMagic errors when a single RHD headstage is parsed against actual FPGA output.
- **建议**：Change the filler formula to `(4 - enabled_streams % 4) % 4` in both `words_per_frame` (protocol.rs line 187) and the matching skip in the parser (parser.rs line 90: `offset = offset.saturating_add((4 - streams % 4) % 4 * 2)`). Update the test fixture in `rhythm_parser.rs` and `simulator_recording.rs` to use the corrected formula, and add a `words_per_frame(1)` assertion that covers the single-stream case explicitly.

#### `H7` [memory-safety] export_intan_rhd buffers entire recording in memory — OOM on real sessions
- [ ] **定位**：`crates/kv-recorder/src/export_formats.rs:93-104`　**置信度**：high　**子系统**：recorder
- **问题**：Two `Vec`s (`all_samples: Vec<i16>` and `timestamps: Vec<u32>`) accumulate every sample of the full recording before any data is written. At 64 channels x 30 kHz, one hour of data requires ~6.9 billion i16 values (~13.8 GB) for `all_samples` plus ~27.6 GB for `timestamps`, totalling ~41 GB — far beyond available RAM on any realistic workstation. The OOM will kill the process and leave a partially-created `.rhd` file.
- **影响**：Any practical-length RHD export crashes the process. Even a 10-minute recording requires ~7 GB RAM. The recording data itself is safe (the `.kvraw` is already on disk) but the export fails without a recoverable error.
- **建议**：Stream blocks directly into RHD data blocks without pre-accumulation. Maintain a ring buffer of at most `RHD_SAMPLES_PER_BLOCK` interleaved frames, fill it from each incoming block, and flush complete 128-frame RHD blocks as they are ready. This reduces memory use from O(total_recording) to O(128 * channel_count).

#### `H8` [project-rule] v1 fallback in KvrawReader::open hardcodes sample_rate=30000 and channel_count=64 — project rule violation
- [ ] **定位**：`crates/kv-recorder/src/lib.rs:1042-1057`　**置信度**：high　**子系统**：recorder
- **问题**：When opening a v1 KVRAW file whose companion `.json` sidecar is absent, `KvrawReader::open` synthesises a `KvrawMetadata` with hardcoded `sample_rate: 30_000.0` and `channel_count: 64`. PROJECT RULE 1 prohibits hardcoding hardware-specific values (ADC conversion factors, channel maps, timestamp clocks) before hardware confirmation. A file from a different device, a different sample rate, or a future hardware revision will be silently decoded with wrong parameters, producing incorrect data with no diagnostic.
- **影响**：Silent data corruption on read: wrong channel mapping, wrong time-axis, wrong amplitude scaling. A user opening an unknown KVRAW file has no indication that the metadata is fabricated.
- **建议**：Return `RecorderError::Io` (or a new `MissingMetadata` variant) instead of fabricating defaults. If a best-effort fallback is truly needed, at minimum log a warning and set `sample_rate` and `channel_count` to 0 so callers detect the unknown state rather than silently consuming wrong values.

#### `H9` [bug] u64::MAX packet_id wraparound kills the streaming pipeline with a false error · 初判 critical
- [ ] **定位**：`crates/kv-integrity/src/lib.rs:113`　**置信度**：high　**子系统**：integrity
- **问题**：`saturating_add(1)` on `u64::MAX` returns `u64::MAX` instead of wrapping to 0. The subsequent less-than check (`current.packet_id < expected_packet_id`) therefore fires for every valid next packet (including 0), returning `IntegrityError::PacketIdWentBackwards`. This error propagates through `drain_streaming` → `PipelineError::IntegrityCheck` and terminates the streaming pipeline in `kv-core/src/pipeline.rs`. At 30 kHz / 64 ch / 64 samples-per-packet the counter reaches `u64::MAX` in ~19.5 million years, making this a theoretical rather than imminent bug — but hardware that restarts its counter, or any test that patches `packet_id` to `u64::MAX`, will hit it immediately. The same defect exists in the batch path (`check_packet_continuity`, line 113) and the incremental path (`IncrementalIntegrity::push`, line 210).
- **影响**：Streaming acquisition pipeline terminates with a fatal error on counter wraparound or any synthetic block with `packet_id = u64::MAX`, corrupting or truncating the recording without the user being aware of the root cause.
- **建议**：Use wrapping arithmetic consistently: `let expected_packet_id = previous.packet_id.wrapping_add(1);`. The gap-count computation on line 123 must use wrapping subtraction too: `current.packet_id.wrapping_sub(expected_packet_id)`. Add a dedicated unit test: `previous_id = u64::MAX`, `current_id = 0` must produce zero gaps and no error.

#### `H10` [project-rule] Hardcoded FPGA bitfile name violates hardware-independence rule
- [ ] **定位**：`crates/kv-cli/src/lib.rs:1194-1201`　**置信度**：high　**子系统**：cli
- **问题**：The function `default_rhd_bitfile_path()` encodes the literal filename `keyvast_260607_with_UART.bit` in source. Project rule 1 explicitly forbids hardcoding FPGA bit-file paths before hardware confirmation, because this couples the compiled binary to one specific build artifact. Any board revision or renamed file silently breaks the default path.
- **影响**：Users running `rhd-smoke` without `--bitfile` will get a 'file not found' error whenever the bit file name changes. The default path is also constructed relative to `CARGO_MANIFEST_DIR` (a compile-time constant), meaning it resolves to a path inside the source tree rather than the working directory or a system-installed location.
- **建议**：Remove the hardcoded filename. Instead, require `--bitfile` to be explicitly supplied (returning `CliError::MissingBitfile` if absent), or read it from an environment variable like `KEYVAST_BITFILE`. Do not embed compile-time source paths in a production binary.

#### `H11` [bug] `--preset` silently discards `--duration` when both are supplied
- [ ] **定位**：`crates/kv-cli/src/lib.rs:1105-1110`　**置信度**：high　**子系统**：cli
- **问题**：In `parse_benchmark_args`, when both `--preset` and `--duration` are given, the arm `(Some(p), Some(_)) => p.duration_seconds()` quietly ignores the user-supplied duration. The user gets no warning and the benchmark runs for the preset duration, not their intended one. This is a silent error — the opposite of project rule 3.
- **影响**：A researcher who writes `benchmark --preset smoke --duration 120` expects a 120-second run for reproducibility but gets a 10-second run instead. Data is collected for the wrong duration with no diagnostic output.
- **建议**：Either: (a) treat `--preset` + `--duration` as a conflict and return `Err(CliError::ConflictingArguments)` with a clear message, or (b) let `--duration` override the preset duration (document the precedence). Neither case should silently discard user input.

#### `H12` [design] No Ctrl-C / signal handling — long-running acquisitions cannot be stopped cleanly
- [ ] **定位**：`crates/kv-cli/src/main.rs:5-93`　**置信度**：high　**子系统**：cli
- **问题**：The `main` function blocks synchronously on `run_command()` with no signal handler installed. For the `benchmark --preset endurance` command (7 200 s run) or any live hardware command, pressing Ctrl-C delivers SIGINT which terminates the process immediately. The streaming recorder's write buffer and any partially written `.kvraw` file are never flushed.
- **影响**：Data loss: samples buffered in the ring buffer at interrupt time are lost. The `.kvraw` file may be left in a partially written state without a valid footer or block count, making it unreadable by downstream tools.
- **建议**：Install a Ctrl-C handler (e.g. via the `ctrlc` crate) that sets an `AtomicBool` cancellation flag. Thread the flag through the pipeline source closure so the acquisition loop exits cleanly and the recorder is flushed before the process terminates. At minimum, document the known data-loss risk in the help text.

#### `H13` [bug] RemoteApiHandle::stop() hard-codes DEFAULT_PORT instead of the handle's actual port
- [ ] **定位**：`crates/kv-gui/src/remote_api.rs:148-154`　**置信度**：high　**子系统**：gui-app
- **问题**：`RemoteApiHandle::stop()` sends a loopback connection to `127.0.0.1:{DEFAULT_PORT}` (4444) to unblock the `accept()` call. However, `start_server` accepts an arbitrary `port` argument and `RemoteApiState` allows the user to change the port. If the server was started on a non-default port (e.g. 5555), the loopback connect goes to the wrong port, does not unblock the listener, the server thread does not exit, and `t.join()` blocks the GUI thread on shutdown until the listener times out or the OS reclaims it.
- **影响**：GUI hangs on shutdown or on toggling the remote API off when the user has set a non-default port. The GUI thread is blocked waiting for the server thread to join, which violates the project rule that GUI failure must not stop acquisition.
- **建议**：Store the actual bound port in `RemoteApiHandle` and use it in `stop()`: `let _ = TcpStream::connect(format!("127.0.0.1:{}", self.port));`. Alternatively, set `SO_REUSEPORT` and use `set_nonblocking(true)` on the listener so the server loop can poll the stop flag without needing a dummy connection.

#### `H14` [bug] Hover amplitude readout ignores user amp_scale — formula omits the gain term
- [ ] **定位**：`crates/kv-gui/src/waveform.rs:457-460`　**置信度**：high　**子系统**：gui-render
- **问题**：finalize_channel applies: y_plot = (y_norm - mean) * gain + y_offset, where gain = DEFAULT_CHANNEL_SPACING * 3.0 * (RHD_FULL_SCALE_UV / amp_scale). The hover readout at line 460 inverts only the constant factor RHD_FULL_SCALE_UV / (3 * DEFAULT_CHANNEL_SPACING), which is valid only when amp_scale equals RHD_FULL_SCALE_UV—roughly 6 390 µV. At any other amp_scale setting the readout is wrong by the ratio (RHD_FULL_SCALE_UV / amp_scale). For the default amp_scale of 500 µV the displayed value is about 12.8× too large.
- **影响**：Clinically or experimentally significant: a spike amplitude shown as 5 mV may actually be 390 µV. Users relying on the hover tooltip to estimate amplitude will record systematically wrong values in lab notebooks.
- **建议**：The correct inversion of finalize_channel is: amp_uv = delta_y / gain * RHD_FULL_SCALE_UV, where gain is the same value computed at line 126. Either thread gain down into the hover block or recompute it locally: let gain = DEFAULT_CHANNEL_SPACING * 3.0 * (RHD_FULL_SCALE_UV / amp_scale.max(1.0)); let amp_uv = delta_y / gain * RHD_FULL_SCALE_UV;

#### `H15` [bug] Spike-count refractory period uses full hardware sample rate to index ring-decimated slot array
- [ ] **定位**：`crates/kv-gui/src/waveform.rs:684-693`　**置信度**：high　**子系统**：gui-render
- **问题**：The per-frame spike detector in collect_from_ring iterates over pts, a Vec of ring slots at effective rate Fs/dwnsp (7 500 Hz at 30 kHz/4). The refractory period is computed as (sample_rate * 0.001).max(1.0) as usize, where sample_rate = block.sample_rate = 30 000 Hz, giving 30 slot-indices. The correct 1 ms refractory at 7 500 Hz ring rate is 7–8 slots, not 30. The refractory period is therefore 4× too long, suppressing detection of spikes closer than ~4 ms apart instead of the intended ~1 ms.
- **影响**：Legitimate spike pairs within 1–4 ms are silently dropped, causing under-counting in the badge and incorrect threshold-crossing detection in the waveform overlay. In high-firing-rate channels (>250 Hz) the badge may read 0 indefinitely.
- **建议**：Replace with: let ring_rate = ring.sample_rate / ring.dwnsp as f64; let refractory = (ring_rate * 0.001).max(1.0) as usize; Add ring as a parameter to collect_from_ring (it is already passed as the first argument) and access ring.dwnsp.

#### `H16` [security] Remote API server binds to 0.0.0.0 with no authentication · 初判 critical
- [ ] **定位**：`crates/kv-gui/src/remote_api.rs:166`　**置信度**：high　**子系统**：gui-support
- **问题**：The TCP server binds to 0.0.0.0 (all interfaces), making acquisition control — start/stop recording, change output directory, change display mode — reachable from any host on the local network (or beyond, depending on the OS firewall). There is no authentication token, no IP allowlist, and no TLS. Any process that can reach the port can start or stop a recording and redirect the output path.
- **影响**：Any network peer can stop an active acquisition, start an unwanted recording to an attacker-controlled path (after validate_output_dir, but still writable anywhere without '..' components), or DoS the server. In a shared-lab or cloud-hosted environment this is a direct data-integrity and security breach.
- **建议**：Bind to 127.0.0.1 instead of 0.0.0.0 unless the user explicitly enables remote access, and add a shared-secret token check on every incoming request. A minimal fix: change the bind address to `127.0.0.1:{port}` and document that external access requires an SSH tunnel or user opt-in.

#### `H17` [bug] RemoteApiHandle::stop uses hardcoded DEFAULT_PORT instead of bound port
- [ ] **定位**：`crates/kv-gui/src/remote_api.rs:148-154`　**置信度**：high　**子系统**：gui-support
- **问题**：RemoteApiHandle::stop connects to 127.0.0.1:DEFAULT_PORT (4444) to unblock the accept() call, but the server may have been started on a different port passed to start_server(). If the user changed the port in the GUI and the server was restarted on that port, stop() connects to the wrong port, fails silently (the result is discarded with let _ = ...), and the server thread blocks forever on accept(), leaking the thread.
- **影响**：Server thread leak on every non-default-port shutdown. The thread holds the TcpListener socket, which blocks the OS port until the process exits.
- **建议**：Store the bound port in RemoteApiHandle and use it in stop(): add a `port: u16` field, set it in start_server(), and change the connect call to `format!("127.0.0.1:{}", self.port)`. Alternatively, set SO_REUSEADDR and use a non-blocking listener with a short accept timeout so the stop flag is polled without a self-connect.

#### `H18` [bug] notch_idx is not bounds-checked after loading from config — panics in production
- [ ] **定位**：`crates/kv-gui/src/config_persist.rs:192`　**置信度**：high　**子系统**：gui-support
- **问题**：apply_to() applies time_scale_idx and amp_scale_idx with .min(array.len() - 1) guards, but notch_idx is written directly without any bounds check. NOTCH_FREQS has only 2 entries (indices 0 and 1). If a config file on disk contains `"notch_idx": 5` (e.g. written by a future version of the software or hand-edited), notch_freq_hz() will index NOTCH_FREQS[5] out of bounds and panic, crashing the application.
- **影响**：Process crash (index-out-of-bounds panic) the next time notch_freq_hz() is called, which happens in app.rs line 488 every time the filter chain is rebuilt. The crash terminates the acquisition thread if the GUI thread panics, violating project rule #4.
- **建议**：Add `filters.notch_idx = self.notch_idx.min(crate::panels::NOTCH_FREQS.len().saturating_sub(1));` in apply_to(), mirroring the pattern already used for the other index fields.

#### `H19` [testing] RHD probe helper functions have zero unit tests despite complex byte-level logic · 初判 critical
- [ ] **定位**：`crates/kv-rhd/src/backend.rs:1472-1675`　**置信度**：high　**子系统**：tests
- **问题**：Five private free functions drive the hardware bring-up path that determines which SPI port and MISO delay are committed for all subsequent acquisition: `verify_chip_id_in_probe`, `min_stream_railed_fraction`, `amplifier_mean_raw_word`, `probe_chip_id`, and `extract_channel_from_raw`. Each contains independent byte-offset arithmetic that reconstructs the Rhythm frame layout. A bug in any one of them causes the wrong port or delay to be selected, producing the exact half-scale 0x4000 / flat-data failure mode the code was written to prevent. None of these functions have a single test. The parallel frame-byte-stride formula is duplicated four times across these functions with slight differences (magic as 4 words vs. 8 bytes), making silent divergence easy to introduce.
- **影响**：A regression in any of the five functions silently produces corrupt amplifier data during real hardware sessions. The existing rhythm_parser tests exercise the public parser API, not the probe path, so a broken probe helper would only be caught during a live hardware bring-up.
- **建议**：Add unit tests for each probe helper in `crates/kv-rhd/tests/rhythm_parser.rs`. Use the existing `build_raw_block` test helper to construct synthetic Rhythm frames and assert that `verify_chip_id_in_probe` returns `true` when the INTAN string is present, `false` otherwise; that `min_stream_railed_fraction` returns ~1.0 for a block of 0xFFFF words and ~0.0 for mid-scale words; that `probe_chip_id` extracts the correct chip-ID byte; and that `extract_channel_from_raw` agrees sample-by-sample with `parse_rhythm_data_block`. Make the functions `pub(crate)` if needed for testing.

#### `H20` [testing] RhdChipType chip-ID dispatch is completely untested
- [ ] **定位**：`crates/kv-rhd/src/protocol.rs:54-80`　**置信度**：high　**子系统**：tests
- **问题**：`RhdChipType::from_register63` maps the register-63 byte to a chip variant (1→Rhd2132, 2→Rhd2216, 4→Rhd2164) and `streams_per_headstage` determines whether one or two data streams are enabled during acquisition. An error here causes the wrong number of streams to be opened, breaking the frame layout for 64-channel (RHD2164) headstages. No test exercises any of these three methods or the `channel_count` accessor.
- **影响**：A bug in `from_register63` (e.g., mixing up values 2 and 4) would cause the backend to open one stream for a 64-channel chip, silently discarding half the channels, or two streams for a 32-channel chip, producing corrupted data with no error.
- **建议**：Add a test in `crates/kv-rhd/tests/rhd_command_lists.rs`: assert that `RhdChipType::from_register63(1)` is `Some(Rhd2132)` with `streams_per_headstage() == 1`, `from_register63(4)` is `Some(Rhd2164)` with `streams_per_headstage() == 2`, `from_register63(0)` is `None`, and so on for all documented values.

#### `H21` [project-rule] CI is workflow_dispatch-only and never runs automatically — contradicts the docs that claim it runs on every PR and push
- [ ] **定位**：`.github/workflows/ci.yml:5-6`　**置信度**：high　**子系统**：docs-build-ci
- **问题**：The CI workflow triggers solely on `workflow_dispatch` (manual button). It does NOT run on `push` or `pull_request`, so tests and clippy never execute unless a human manually starts them from the Actions tab. Meanwhile docs/15-dev-handoff.md Session 25 (lines 142-144) documents this exact workflow as running 'two jobs on every PR and pushes to main/v2.0'. The repo therefore has effectively no automated regression protection, and the handoff note is actively misleading about that. Given the active gui-ux branch and recent hardware-bring-up commits, regressions in kv-core/kv-rhd/kv-recorder can land with no gate.
- **影响**：No automated test/lint gate on commits or PRs; broken builds, failing tests, or new clippy warnings can merge undetected. Developers reading the handoff believe CI guards every PR when it does not.
- **建议**：Either restore automatic triggers (`on: [push, pull_request]` or scoped to the active branches) or, if manual-only is intentional, update docs/15-dev-handoff.md Session 25 to state clearly that CI is manual-only. Do not leave the doc claiming per-PR execution.

#### `H22` [project-rule] .gitignore does not exclude the diagnostic scripts, the 2.6 MB .bit file, captures/, __pycache__/, kvlog.txt, or *.png
- [ ] **定位**：`.gitignore:1-26`　**置信度**：high　**子系统**：docs-build-ci
- **问题**：`git status` shows intan_rec_controller_7310.bit (2,605,421 bytes), compare_kvraw_vs_oe.py, diag_glitch.py, diag_hf.py, diag_plot.py, diag_rails.py, captures/, and __pycache__/ as untracked (??), and `git check-ignore` confirms none of them are ignored. .gitignore only covers /target/, IDE files, .env, root-level acquisition outputs (*.kvraw, recording.json, integrity.json, benchmark.json, log.txt), Thumbs.db/Desktop.ini. It does NOT cover: *.bit, *.py / diag_* scratch scripts, /captures/, __pycache__/, *.png (diag_plot.py writes diag.png), or kvlog.txt (written by gui-log.bat). docs/18-rhd-signal-debug.md §7 explicitly states the diagnostic tooling is 'scratch — not committed', so the intent is clearly that these stay out of git, but nothing enforces it. A routine `git add -A` would commit a 2.6 MB binary FPGA bitstream and scratch scripts.
- **影响**：Accidental commit of a 2.6 MB proprietary FPGA binary and scratch diagnostic scripts, bloating the repo and (for the .bit) violating the hardware-independence rule of keeping bitfiles out of the tracked source base. __pycache__ noise and per-run captures/ output could also be committed.
- **建议**：Add to .gitignore: `*.bit`, `__pycache__/`, `*.pyc`, `/captures/`, `*.png` (or a scoped `diag*.png`), and `kvlog.txt`. Decide explicitly whether the diagnostic .py scripts should be committed under a `tools/` or `scripts/` dir (with the hardcoded path fixed) or ignored; right now they are in an ambiguous limbo that the docs say should be 'not committed'.

#### `H23` [memory-safety] export_kvraw accumulates entire recording as Vec<SampleBlock> despite appearing to chunk
- [ ] **定位**：`crates/kv-gui/src/app.rs:2354-2383`　**置信度**：medium　**子系统**：completeness-critic
- **问题**：The comment at line 2354 says 'Read in ~1 s chunks; the exporters re-chunk internally as needed,' but the code pushes every chunk into a `blocks: Vec<SampleBlock>` accumulator that grows until the entire file has been read, then passes the full Vec to the exporter. A 1-hour, 64-channel, 30 kHz recording produces approximately 13.8 GB of raw i16 data plus Vec/SampleBlock overhead. The read_frames call also allocates a Vec<i16> per chunk. The comment is misleading: chunking on read does not help because nothing is ever flushed or discarded before the export call.
- **影响**：Any real-length recording export will exhaust system RAM and either crash the GUI (OOM kill) or cause virtual-memory thrashing that renders the system unresponsive. Since the export runs in a background thread, it also has no progress reporting capability while building the in-memory buffer.
- **建议**：Pass an iterator or streaming callback to the exporters rather than a Vec. The export_intan_rhd and export_flat_binary functions should accept impl Iterator<Item = &SampleBlock> and write each block as it arrives. Alternatively, change the GUI-side loop to open the output file first and call a block-by-block append API on the exporter.

#### `H24` [bug] KvrawReader::read_frames uses unchecked usize arithmetic that overflows on large files
- [ ] **定位**：`crates/kv-recorder/src/lib.rs:1107-1110`　**置信度**：medium　**子系统**：completeness-critic
- **问题**：read_frames computes `let total_samples = ch * num_frames` and `let byte_count = total_samples * 2` using plain `usize` multiplication with no overflow check. On a 64-bit target at FRAMES_PER_CHUNK=30_000 and ch=64 these values are within usize range, but the computation for the seek offset `let sample_offset = start_frame * ch as u64` also uses plain u64 multiplication. For pathologically large files or for long sessions where start_frame approaches u64::MAX/64, this overflows in debug mode (panic) or wraps silently in release mode, causing a seek to a wrong file position and reading garbage data.
- **影响**：Corrupt data read during export or playback for recordings longer than roughly 2^57 seconds (not practically reachable today), but the immediate concern is the `(max_frames - start_frame) as usize` truncation at line 1101, which silently narrows a u64 to usize on a hypothetical 32-bit build and could underflow if max_frames < start_frame due to the earlier `.min(max_frames)` not being applied yet when start_frame arrives from an untrusted source.
- **建议**：Use checked_mul for all arithmetic: `ch.checked_mul(num_frames).ok_or(RecorderError::Overflow)?` and similarly for byte_count and sample_offset. Gate `(max_frames - start_frame) as usize` with a check that the difference fits in usize.

## 5. 🟡 Medium（43）

#### `M1` [design] Documented DeviceBackend trait does not exist; real abstraction is the thinner AcquisitionSource
- [ ] **定位**：`docs/03-architecture.md:43-67`　**置信度**：high　**子系统**：architecture
- **问题**：The architecture doc presents a DeviceBackend trait as the interface all devices satisfy, and rule 1 names it a stable contract. No such trait exists in code; only the unrelated DeviceBackendKind enum. The actual abstraction is kv_core AcquisitionSource with a single read_block. Backends are unified by hand: kv-gui via an ActiveSource enum, kv-cli via per-backend closures.
- **影响**：The stable backend contract is only the read path; lifecycle is duplicated per call site. New backends cannot be dropped in polymorphically, and docs misrepresent the extension point.
- **建议**：Introduce a real DeviceBackend trait in kv-core both backends implement, or update docs to state AcquisitionSource is the actual contract and lifecycle is backend-specific.

#### `M2` [performance] FanoutBlockBuffer push() clones the block into the preview channel then also pushes into fanout — one extra heap allocation per block on the hot path
- [ ] **定位**：`crates/kv-gui/src/live_pipeline.rs:247-258`　**置信度**：high　**子系统**：types-buffer
- **问题**：In producer_loop(), each block is cloned for the preview mpsc channel (block.clone() on line 247, which deep-copies the entire Vec<i16> payload), and the original is then pushed into the fanout buffer which wraps it in Arc::new(block) (kv-buffer/src/lib.rs line 105). This means every block incurs two heap allocations: one for the preview clone and one for the Arc. If the fanout push were performed first and the resulting Arc sent to the preview channel instead (or the preview channel were added as a regular fanout consumer), only one Arc::new() allocation would occur per block. At 64ch × 30 kHz / 64 spp = ~469 blocks/s this generates ~469 unnecessary full buffer allocations per second.
- **影响**：Unnecessary allocation pressure on the acquisition hot path. Each SampleBlock data Vec is 64 channels × 64 samples × 2 bytes = 8 KB; 469 gratuitous clones/s = ~3.75 MB/s of unnecessary allocation. This is below the threshold for catastrophic failure but will increase GC pressure, cache churn, and acquisition jitter.
- **建议**：Wrap the block in Arc::new() before the try_send call, then send Arc::clone(&arc) to the preview channel and push the arc into the fanout. This requires the preview channel type to change from mpsc::SyncSender<SampleBlock> to mpsc::SyncSender<Arc<SampleBlock>>, or alternatively add the preview channel as a second fanout consumer so it benefits from the same Arc sharing the fanout already performs.

#### `M3` [concurrency] The same lock-while-doing-I/O issue exists in run_threaded_pipeline via drain_consumer (clone inside lock) · 初判 high
- [ ] **定位**：`crates/kv-core/src/pipeline.rs:179-183`　**置信度**：high　**子系统**：core-pipeline
- **问题**：drain_consumer calls (*block).clone() on each Arc<SampleBlock> while the MutexGuard is still held. A SampleBlock contains a Vec<i16> of channel samples (64 channels x samples_per_packet = potentially hundreds of bytes). The clone is proportional to block size and number of queued blocks. At high channel counts or large samples_per_packet this is non-trivial allocation work under the lock. The producer is blocked for the full clone duration of every drained block.
- **影响**：Producer backpressure accumulates during drain; with large blocks or high queue depth the acquisition thread is effectively stalled on the consumer's allocator, increasing the probability of ring-buffer overflow and silent drops in the preview consumer (capacity 2-16 blocks in tests).
- **建议**：Drain only the Arc pointers inside the lock (O(pointer-size) per block), then clone the SampleBlock data outside the lock after releasing the MutexGuard. Since Arc<SampleBlock> is reference-counted and the producer only writes through push (which also Arc-wraps), it is safe to hold the Arc after dropping the guard and clone the inner value without the lock.

#### `M4` [memory-safety] unsafe blocks in process_metrics.rs lack SAFETY comments · 初判 high
- [ ] **定位**：`crates/kv-core/src/process_metrics.rs:115-123 and 139-148`　**置信度**：high　**子系统**：core-pipeline
- **问题**：Three unsafe blocks call Win32 FFI functions (GetProcessTimes, GetProcessMemoryInfo) and use std::mem::zeroed() on a C struct. None of them carry a // SAFETY: comment documenting the invariants being upheld. The project's AGENTS.md rules and the Rust reviewer guidelines both mandate a // SAFETY: comment for every unsafe block. Additionally, std::mem::zeroed::<PROCESS_MEMORY_COUNTERS>() is sound only if all-zeros is a valid bit pattern for every field of that struct — this is likely true for the Windows SDK type but is undocumented here.
- **影响**：Future maintainers cannot verify the soundness of these calls, and automated auditing tools (e.g. cargo geiger) will flag these as unreviewed. If the struct layout changes in a future windows-sys version, the zeroed() call could produce an invalid struct silently.
- **建议**：Add a // SAFETY: comment before each unsafe block. For example: // SAFETY: GetCurrentProcess() returns a pseudo-handle that is always valid for the calling process. All output FILETIME pointers are valid stack allocations. For zeroed(): // SAFETY: PROCESS_MEMORY_COUNTERS is a C struct whose zero-bit representation is defined by the Windows SDK as a valid initial state; cb is set immediately after.

#### `M5` [error-handling] Producer error is checked after already consuming all blocks, causing misleading partial-success state
- [ ] **定位**：`crates/kv-core/src/pipeline.rs:185-193 and 330-342`　**置信度**：high　**子系统**：core-pipeline
- **问题**：When producer_done is true the code drains all remaining buffered blocks first (line 186/331), then checks producer_error (line 189/339). If the producer failed partway through, the error path returns Err(ProducerFailed) but has already moved data into recorded_blocks (or written it to disk) without any record of how many blocks were successfully acquired before the failure. The error type PipelineError::ProducerFailed carries no partial-result or block-count field, so callers cannot distinguish 'failed after 0 blocks' from 'failed after 999 blocks'.
- **影响**：A caller recovering from a ProducerFailed error has no way to know whether a partial recording was written to disk or how many blocks are valid. For the streaming pipeline, partial data may have been flushed to the .kvraw file with no metadata indicating truncation, leading to a silently corrupt recording if the caller does not handle this carefully.
- **建议**：Add a blocks_acquired: u64 field to PipelineError::ProducerFailed so callers can log or surface how much data was collected before failure. For the streaming pipeline, consider calling recorder.finish() even on the error path and including the partial RecordingSummary in the error, so the .kvraw file is at least correctly finalized (header updated) before returning the error.

#### `M6` [bug] spike_component fires a burst across an entire packet rather than a single sparse spike
- [ ] **定位**：`crates/kv-simulator/src/lib.rs:261-279`　**置信度**：high　**子系统**：simulator
- **问题**：The spike event seed is `seed ^ (sample_index / DEFAULT_SAMPLES_PER_PACKET) ^ (channel * 17)`. The division result is constant for all 64 samples within a given packet. When `mix_u64(event_seed) % rarity == 0`, the condition is true for ALL samples in that packet where `sample_index % 6 <= 2`, which is ~33 of 64 samples. The result is a repeating 3-sample biphasic template (-180, 260, -80, -180, 260, -80 ...) emitted 10-11 times in succession across one packet — not a single 3-sample spike. This misrepresents neural spiking activity.
- **影响**：Any downstream code exercising spike detection, threshold crossing, or spike sorting with simulator data will see highly unrealistic bursting activity and may produce misleading benchmark or validation results. Testing downstream algorithms against this data will not represent real hardware behavior.
- **建议**：Gate the spike template on a per-event absolute sample position rather than a per-packet flag. For example, derive an event sample index from the packet-level seed: `let event_sample = mix_u64(event_seed) % (rarity * DEFAULT_SAMPLES_PER_PACKET as u64);` and only emit the template when `sample_index` is within 3 samples of that position. This produces a genuine isolated biphasic waveform.

#### `M7` [design] triangle_wave period hardcoded to DEFAULT_SAMPLES_PER_PACKET regardless of configured packet size
- [ ] **定位**：`crates/kv-simulator/src/lib.rs:249-259`　**置信度**：high　**子系统**：simulator
- **问题**：`triangle_wave` computes its period as `DEFAULT_SAMPLES_PER_PACKET * 4 = 256` regardless of what `config.device.samples_per_packet` is. If a caller configures, say, `samples_per_packet = 128`, the LFP waveform still repeats every 256 samples (2 full periods per packet) instead of scaling proportionally. This also means the LFP frequency in Hz depends on the packet size only indirectly, violating the principle that signal frequency should be independent of transport packet sizing.
- **影响**：Any test or benchmark that changes `samples_per_packet` to stress-test packet boundary handling will observe a different LFP frequency relative to the sample rate, producing inconsistent data that may confuse downstream processing. It also makes it impossible to simulate a stable 1 Hz LFP across different packet configurations.
- **建议**：Pass `sample_rate` and a desired LFP frequency (e.g. 8.0 Hz) as parameters, and compute `period = (sample_rate / lfp_hz) as u64`. Alternatively, accept `samples_per_packet` and scale accordingly. This decouples waveform frequency from transport packet size.

#### `M8` [performance] No real-time pacing: simulator produces blocks at unbounded CPU speed
- [ ] **定位**：`crates/kv-simulator/src/lib.rs:52-87`　**置信度**：high　**子系统**：simulator
- **问题**：`next_block()` returns immediately with no wall-clock delay. When used in the pipeline producer thread, the simulator loops calling `next_block()` as fast as the CPU allows, burning 100% of one core and producing blocks at potentially hundreds of thousands of packets per second rather than the target ~468 packets/sec (30 kHz / 64 samples per packet). There is no sleep, token bucket, or deadline-based pacing anywhere in the simulator or the producer thread.
- **影响**：Benchmark throughput figures measured with the simulator do not reflect any real-time constraint, making them misleading as a proxy for real hardware behavior. Buffer overflows, back-pressure, and ring-buffer behavior all look different at simulated speed vs. real-time. Any code that assumes the producer runs at approximately the declared `sample_rate` will behave incorrectly when tested against this simulator.
- **建议**：Add an optional real-time pacing mode. One approach: record the wall-clock time of the first `next_block()` call and, on each subsequent call, compute the expected wall-clock time for the next packet (`packet_id * samples_per_packet / sample_rate`) and sleep until that deadline using `std::thread::sleep(deadline.saturating_duration_since(Instant::now()))`. This should be opt-in (e.g. a `paced: bool` field in `SimulatorConfig`) so fast-forwarding tests can still run unconstrained.

#### `M9` [design] Frame-layout arithmetic is duplicated and diverges from the parser — fragile and already inconsistent · 初判 high
- [ ] **定位**：`crates/kv-rhd/src/backend.rs:1481-1489, 1523-1548, 1576-1581, 1614-1619, 1652-1658`　**置信度**：high　**子系统**：rhd-hardware
- **问题**：Five private analysis functions (verify_chip_id_in_probe, min_stream_railed_fraction, extract_channel_from_raw, amplifier_mean_raw_word, probe_chip_id) each independently hard-code the Rhythm frame layout — magic size, timestamp size, aux ordering, amplifier ordering, padding, board ADC count, TTL words — producing five separate instances of: frame_bytes = (4 + 2 + enabled_streams*(CHANNELS_PER_STREAM+3) + (enabled_streams%4) + 8 + 2) * 2. These diverge in subtle ways from parser.rs, which is the authoritative layout. For example, verify_chip_id_in_probe's comment says 'magic(8)' (8 bytes) but the frame_bytes formula uses 4 words = 8 bytes — it happens to be correct but the comment unit mismatch (words vs bytes) makes auditing error-prone. The aux layout comment in verify_chip_id_in_probe (line 1478) describes the frame in bytes but the offset formula mixes bytes and words.
- **影响**：Any future change to the frame format (e.g. adding a field, changing magic size) must be updated in at least six places (the five functions plus parser.rs) and a miss in any one of them causes silent data corruption during MISO scanning or impedance measurements. This is a data-integrity risk that grows with the codebase.
- **建议**：Extract a FrameLayout struct (or at least named constants) computed once from enabled_streams, shared by all analysis functions and the parser. Provide unit-tested helper methods frame_bytes(streams), auxcmd3_byte_offset(streams, stream_index), amp_byte_offset(streams, channel, sample) that every consumer calls rather than recomputing inline. This eliminates the six-way duplication and makes future protocol changes a one-line fix.

#### `M10` [project-rule] backend.rs exceeds the project 800-line file size rule
- [ ] **定位**：`crates/kv-rhd/src/backend.rs:1-1676`　**置信度**：high　**子系统**：rhd-hardware
- **问题**：The file is 1676 lines — more than twice the 800-line project-rule cap. It contains at least four distinct responsibilities: board configuration and bring-up (configure, reset_board, initialize_rhd_chips), the acquisition loop (read_raw_block), the impedance test (run_impedance_test, ~200 lines), and the frame-analysis helpers for MISO scanning (verify_chip_id_in_probe, min_stream_railed_fraction, extract_channel_from_raw, amplifier_mean_raw_word, probe_chip_id, ~250 lines).
- **影响**：The large size makes the file hard to review, extends the blast radius of any bug, and buries the frame-analysis duplication identified in the finding above.
- **建议**：Extract the frame-analysis helpers to crates/kv-rhd/src/frame_analysis.rs (or merge them into protocol.rs as associated functions on a FrameLayout type). Extract run_impedance_test and its support functions to crates/kv-rhd/src/impedance.rs (an impedance.rs stub already exists). Target backend.rs at under 800 lines.

#### `M11` [testing] No unit tests for hardware-critical logic: MISO scan, frame layout helpers, register packing
- [ ] **定位**：`crates/kv-rhd/src/backend.rs:1461-1675`　**置信度**：high　**子系统**：rhd-hardware
- **问题**：All five frame-analysis helpers (verify_chip_id_in_probe, min_stream_railed_fraction, extract_channel_from_raw, amplifier_mean_raw_word, probe_chip_id) and the register assembly in commands.rs have no unit tests. These are the functions most likely to have off-by-one errors in byte offsets, and their correctness determines whether the MISO delay scan finds the right port. The test suite reports 0 unit tests for kv_rhd.
- **影响**：Bugs in the frame-analysis helpers produce wrong port selection or wrong MISO delay during bring-up, resulting in the exact half-scale 0x4000 data corruption the retry logic was written to prevent. With no test coverage, regressions go unnoticed.
- **建议**：Add #[cfg(test)] unit tests that: (1) construct a synthetic raw frame (known magic, timestamp, aux bytes, amp bytes) and assert verify_chip_id_in_probe, extract_channel_from_raw, and amplifier_mean_raw_word return the expected values; (2) verify register_value for known open_ephys_default() register states; (3) verify create_command_list_register_config produces a list of exactly RHD_COMMAND_LIST_LEN commands and starts with reg_read(63). These tests do not require hardware.

#### `M12` [bug] Hardcoded DAC voltage and µV-per-count scale in `compute_impedance` will silently produce wrong results · 初判 high
- [ ] **定位**：`crates/kv-rhd/src/impedance.rs:146-158`　**置信度**：high　**子系统**：rhd-parsing
- **问题**：`compute_impedance` hard-codes `0.195` (µV/count) and `128.0 * 1.225 / 256.0` (DAC peak voltage) as literals, despite `RHD_AMPLIFIER_MICROVOLTS_PER_COUNT` existing in `protocol.rs` and `DEFAULT_DAC_AMPLITUDE` existing in `impedance.rs` itself. The function signature also accepts a `cap_scale` argument and the caller in `backend.rs` passes `config.dac_amplitude` separately — but that field is never forwarded into this function. If the amplitude or conversion factor changes (different RHD variant, calibration), the impedance magnitude will be silently wrong by a constant factor. The project rule explicitly prohibits hardcoded ADC conversion factors before hardware confirmation.
- **影响**：Impedance measurements scale incorrectly with no compile-time or runtime error when configuration changes.
- **建议**：Add `dac_amplitude: f64` and `microvolts_per_count: f64` parameters to `compute_impedance` (or accept an `&ImpedanceTestConfig`), replace the literals with `sample as f64 * microvolts_per_count` and `dac_amplitude * 1.225 / 256.0`, and use `RHD_AMPLIFIER_MICROVOLTS_PER_COUNT` from `protocol.rs` at call sites.

#### `M13` [project-rule] Hardcoded DAC voltage reference (1.225 V) violates project no-hardcoded-ADC-factors rule
- [ ] **定位**：`crates/kv-rhd/src/impedance.rs:157-158`　**置信度**：high　**子系统**：rhd-parsing
- **问题**：The DAC full-scale reference voltage `1.225 V` is a raw literal with no named constant, no comment citing the Intan datasheet section, and no flag that this value must be confirmed against the actual AVDD supply on the board. The project AGENTS.md rule explicitly states 'Do not hardcode ... ADC conversion factors ... before hardware confirmation'. The RHD2000 datasheet gives this as a nominal value, but it varies with the actual AVDD supply.
- **影响**：Impedance readings will be systematically off if the board's AVDD differs from 1.225 V, with no way for users to correct the value without editing source.
- **建议**：Extract `1.225_f64` into a named constant `RHD_DAC_VREF_VOLTS` in `protocol.rs` with a doc comment citing Intan RHD2000 datasheet table and noting it is AVDD-dependent, then reference it in `compute_impedance`.

#### `M14` [data-integrity] Incomplete JSON escaping of `notes` field in flat binary metadata writer · 初判 high
- [ ] **定位**：`crates/kv-recorder/src/export_formats.rs:314-315`　**置信度**：high　**子系统**：recorder
- **问题**：The flat binary metadata builder escapes `notes` with only `notes.replace('"', "\\"")` before interpolating into the JSON format string. This does not escape backslashes, newlines (`\n`), carriage returns (`\r`), or tabs (`\t`). A notes string such as `C:\Users\data` or a multi-line note produces invalid JSON in `recording.meta.json`, which will fail to parse in any downstream tool. By contrast, `lib.rs` contains the correct `escape_json_string` helper that handles all of these cases but it is not used here.
- **影响**：Downstream tools (Python loaders, SpikeGLX readers) will reject or misparse the companion metadata file. The recorded binary data is unaffected but the sidecar is corrupted.
- **建议**：Replace `notes.replace('"', "\\\"")`  with `escape_json_string(notes)` (the function already exists in `lib.rs`). Make `escape_json_string` pub(crate) and import it from `export_formats.rs`, or move it to a shared internal module.

#### `M15` [performance] `write_latencies_us` Vec grows without bound for the lifetime of a StreamingRecorder session
- [ ] **定位**：`crates/kv-recorder/src/lib.rs:596`　**置信度**：high　**子系统**：recorder
- **问题**：Every call to `write_block` appends one `u64` to `self.write_latencies_us`. At 30 kHz with 64 samples/packet this is ~469 blocks/second, or ~1.69 million entries per hour. A one-hour session accumulates ~13.5 MB in the recorder thread's heap. There is no cap, rolling window, or reservoir. For long overnight recordings this approaches 100+ MB and violates the project rule that hot-path state must avoid unbounded latency/allocation.
- **影响**：Memory growth proportional to session duration; on multi-hour recordings this becomes a significant unintentional memory leak in the acquisition thread. The data is only consumed once at `finish()` and sorted there (another O(n log n) operation).
- **建议**：Use a fixed-size reservoir sampler (e.g., 65536 slots with random replacement) or track running min/max/mean/p-tiles with an online algorithm (e.g., P² or t-digest). This caps memory at O(1) regardless of session length.

#### `M16` [data-integrity] Every packet gap also fires a spurious timestamp discontinuity, inflating that counter · 初判 high
- [ ] **定位**：`crates/kv-integrity/src/lib.rs:136-153`　**置信度**：high　**子系统**：integrity
- **问题**：When one or more packets are missing, the timestamp of the next received block necessarily jumps by `missing_count * samples_per_channel`. The code records the packet gap correctly in `check_packet_continuity` but then unconditionally also calls `check_timestamp_continuity`, which fires because the timestamp did not advance by exactly `previous.samples_per_channel`. The result is that `IntegritySummary.timestamp_discontinuities` counts N_gaps + N_genuine_clock_discontinuities rather than just genuine clock discontinuities. The batch path (lines 86-87) and the incremental path (`push`, lines 245-260) both share this flaw.
- **影响**：The integrity report overstates `timestamp_discontinuities`, making it impossible for downstream consumers (e.g., a researcher reviewing a session) to distinguish real clock glitches from normal packet loss. The project rule against silent error misclassification is violated.
- **建议**：Suppress the timestamp check when a packet gap is detected. One clean approach: return a `bool` from `check_packet_continuity` indicating whether a gap occurred, and skip `check_timestamp_continuity` when it returns `true`. Alternatively compute the expected timestamp accounting for the full span of missing packets: `expected_timestamp = previous.timestamp_start + (1 + missing_count) * previous.samples_per_channel`, and only flag a discontinuity if the observed timestamp deviates from that.

#### `M17` [performance] `expected_missing_samples` in batch path is O(n * g) — quadratic for large gap counts
- [ ] **定位**：`crates/kv-integrity/src/lib.rs:97-103`　**置信度**：high　**子系统**：integrity
- **问题**：After the main loop, `check_blocks` iterates over all `packet_gaps` and for each calls `expected_missing_samples`, which scans the entire `blocks` slice to find the block preceding the gap. This is O(n) per gap, O(n * g) overall. While `g` is small in normal operation, a session with heavy packet loss (burst USB drop, overloaded buffer) can produce many gaps. With 30 kHz acquisition and large blocks this can run for tens of milliseconds on report finalization, which is on the write/report path.
- **影响**：Report finalization latency scales quadratically with packet loss severity — worst-case when loss is highest and latency matters most.
- **建议**：Build a `HashMap<u64, usize>` (packet_id → samples_per_channel) from the blocks slice once before the loop, then use O(1) lookup in `expected_missing_samples`. Alternatively, track `previous_samples_per_channel` alongside the gap recording during the loop (mirroring what `IncrementalIntegrity::push` already does correctly).

#### `M18` [testing] No test coverage for counter wraparound, multi-stream mixing, or large gap estimation
- [ ] **定位**：`crates/kv-integrity/tests/integrity_report.rs:1-244`　**置信度**：high　**子系统**：integrity
- **问题**：The 10 existing tests cover only: empty input, one gap, one timestamp discontinuity, invalid block, and basic batch-vs-incremental equivalence for the no-loss case. Missing coverage: (1) `packet_id = u64::MAX` wraparound (the critical bug above), (2) blocks from two different `stream_id` values interleaved, (3) a gap at the very first comparison (between packets 0 and 2), (4) multiple consecutive gaps, (5) `check_blocks` vs `IncrementalIntegrity` equivalence when a gap is present (the existing equivalence test only runs on a no-gap sequence), (6) `finish()` called on an empty `IncrementalIntegrity` after no `push` calls.
- **影响**：The critical wraparound bug and the spurious-discontinuity bug are not caught by any test. Regressions in gap-count arithmetic will go undetected.
- **建议**：Add dedicated test cases: (1) two blocks with `packet_id` 0 and `u64::MAX` then 0 again to verify wrapping; (2) a pair from stream 0 followed by a pair from stream 1 fed to a single `IncrementalIntegrity` to document current behavior; (3) a sequence with three gaps to verify `expected_samples` arithmetic; (4) equivalence test for batch vs incremental on a sequence that includes packet loss.

#### `M19` [project-rule] Hardcoded cable length and device-id string violate hardware-independence rule · 初判 high
- [ ] **定位**：`crates/kv-cli/src/lib.rs:546, 591`　**置信度**：high　**子系统**：cli
- **问题**：Two hardware-specific constants are embedded directly in the `run_rhd_smoke` function body: the device ID string `"rhd-xem7310"` and the cable length value `0.9144` meters. Project rule 1 forbids hardcoding hardware-specific parameters (register maps, ADC factors, channel maps, etc.) that have not been confirmed with real hardware.
- **影响**：Any change to the board model or cable length requires a source-code edit and recompilation. The cable length in particular affects signal integrity tuning and should be configurable at runtime.
- **建议**：Expose `--cable-length` as a CLI argument for `rhd-smoke` with a clearly documented default, and move the device-id to a named constant in `kv-rhd` (or derive it from the hardware). Add a `--device-id` flag if multi-board configurations are planned.

#### `M20` [quality] `cargo fmt --check` fails — code is not formatted to rustfmt standard
- [ ] **定位**：`crates/kv-cli/src/lib.rs:1-10, 164-190, 558-601`　**置信度**：high　**子系统**：cli
- **问题**：Running `cargo fmt --check` produces diffs in both `lib.rs` and `main.rs` (import ordering, enum variant brace style, line-wrapping). The project requires clean formatting as a CI gate.
- **影响**：CI will fail on this branch. Other contributors applying rustfmt will produce noisy diffs that obscure real changes.
- **建议**：Run `cargo fmt` before committing. Add a `cargo fmt --check` step to CI that blocks merges.

#### `M21` [data-integrity] Hardcoded zero timestamps in all AcquisitionEvent records
- [ ] **定位**：`crates/kv-cli/src/lib.rs:775, 787, 828, 840`　**置信度**：high　**子系统**：cli
- **问题**：Every `AcquisitionEvent::Started` and `AcquisitionEvent::Stopped` record is emitted with `timestamp_host_ms: 0`. The field exists precisely to record when acquisition started and stopped against a wall clock, but the real time is never captured.
- **影响**：The `events.csv` file — which is supposed to be the audit trail for recording provenance — contains bogus timestamps for the session start/end. Any analysis tool or data integrity check that relies on these timestamps will produce incorrect results.
- **建议**：Record `SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)` at the point where acquisition starts and again when it stops, and supply those values to the event records.

#### `M22` [quality] lib.rs is 1 288 lines — exceeds 800-line project limit
- [ ] **定位**：`crates/kv-cli/src/lib.rs:1-1288`　**置信度**：high　**子系统**：cli
- **问题**：The project coding rules cap files at 800 lines. At 1 288 lines, lib.rs mixes argument parsing, command dispatch, hardware configuration, benchmark summarisation, log-line generation, and a home-rolled date formatter into a single file.
- **影响**：Reduced readability and maintainability. Adding new commands or fixing a bug in one area requires navigating all areas of the file. The date computation logic (`civil_date_from_unix_days`) in particular is unrelated to CLI concerns and has no unit tests within the module.
- **建议**：Extract into at minimum three modules: `args.rs` (parsing only), `commands.rs` (run_* functions), and `report.rs` (benchmark/log/event summarisation helpers). Move `run_directory_name_utc` and `civil_date_from_unix_days` to a shared `util.rs` or use the `time` / `chrono` crate.

#### `M23` [concurrency] Unbounded remote API command/response queues with no depth cap · 初判 high
- [ ] **定位**：`crates/kv-gui/src/remote_api.rs:132-134`　**置信度**：high　**子系统**：gui-app
- **问题**：`CommandQueue` and `ResponseQueue` are `Arc<Mutex<VecDeque<...>>>` with no capacity limit. The client threads push commands at line 257 without checking queue depth. If many TCP clients connect and flood the server with commands faster than the GUI's 60 Hz frame rate can drain them, the VecDeque grows without bound. Similarly, if a client disconnects without reading its response, the response stays in the queue forever (it is only removed when the *same* id is matched). The `handle_client` timeout logic (line 274) only sends a timeout error to the *client*; it does not remove the unmatched response from the queue.
- **影响**：Memory exhaustion under adversarial client behaviour. Leaked responses accumulate across all sessions. On a long-running acquisition system this is a practical OOM risk.
- **建议**：Cap the command queue (e.g. 64 entries) and return a JSON-RPC error to the client when the cap is exceeded. In `handle_client`, remove the unmatched response from the queue after the 100 ms timeout (currently the response stays if `response_sent == false`). Add a periodic cleanup sweep in the GUI's `tick_device` that drops responses older than a few seconds.

#### `M24` [memory-safety] export_kvraw loads entire recording into memory before writing
- [ ] **定位**：`crates/kv-gui/src/app.rs:2340-2400`　**置信度**：high　**子系统**：gui-app
- **问题**：`export_kvraw` accumulates all decoded `SampleBlock` objects in a `Vec<SampleBlock>` before passing them to the exporter. For a 64-channel, 30 kHz, 1-hour recording that is ~13 GB of i16 samples, plus the overhead of the SampleBlock structs. The function runs on a background thread so it cannot crash the GUI directly, but the system will OOM-kill the process and terminate ongoing acquisition.
- **影响**：OOM kill of the entire process during large-file export, terminating acquisition and potentially corrupting the recording in progress.
- **建议**：Restructure `export_kvraw` to use a streaming/chunked approach: pass each chunk to the exporter incrementally (open the export output once before the loop, then write+flush each chunk and discard it). This requires the exporter APIs to support incremental writes, but that is the correct long-term design for large-file export.

#### `M25` [design] app.rs is 2403 lines — far over the 800-line project file size limit
- [ ] **定位**：`crates/kv-gui/src/app.rs:1-2403`　**置信度**：high　**子系统**：gui-app
- **问题**：The project's coding rules state a hard 800-line maximum per file. `app.rs` is 2403 lines (3x the limit) and contains: the `KvApp` struct definition, the `new` constructor, all acquisition lifecycle methods (start_demo, start_device, stop_all, begin_recording, stop_recording, toggle_acquisition), the demo tick, the device tick with inline recorder-event processing and remote-API command dispatch, all keyboard handlers, the full egui `update()` rendering (toolbar, sidebar, central panel, overlays), and the standalone `export_kvraw` function. This single file has very high coupling and low cohesion.
- **影响**：High cognitive load, difficult to review for correctness, makes it hard to test individual subsystems in isolation. Every change touches this file, causing merge conflicts and making it harder to enforce the invariant that GUI rendering cannot affect acquisition.
- **建议**：Extract into focused modules: `acquisition.rs` (start/stop/tick logic), `recording.rs` (begin/stop/demo-write logic), `render/toolbar.rs`, `render/sidebar.rs`, `render/central.rs`, keeping `app.rs` as a thin orchestrator under 300 lines. The `export_kvraw` standalone function should move to `export.rs`.

#### `M26` [bug] Device-mode recording state not reset to Idle when pipeline is dropped via SourceError
- [ ] **定位**：`crates/kv-gui/src/app.rs:1265-1277`　**置信度**：high　**子系统**：gui-app
- **问题**：When a `RecorderEvent::SourceError` is received (line 1265), the code sets `self.live_pipeline = None` and transitions `recording.state = Idle` (line 1273) only if the state is `Recording`. If the recording state is `Armed` when the device error occurs, `recording.state` stays `Armed` indefinitely. The user sees 'ARMED' in the toolbar and the record button remains active even though there is no pipeline and no way to start a recording.
- **影响**：Stale 'ARMED' UI state after device disconnection. The user cannot start acquisition again without noticing the stuck state, and pressing R would call `begin_recording` with no pipeline, silently doing nothing (Device branch of `begin_recording` checks `live_pipeline.is_some()`).
- **建议**：Change the condition to reset the recording state for both `Recording` and `Armed`: `if self.recording.state != RecordingState::Idle { self.recording.state = RecordingState::Idle; ... }`.

#### `M27` [testing] No tests for ingest_block, filter_block_with_chains, or tick_device · 初判 low
- [ ] **定位**：`crates/kv-gui/src/app.rs:1-2403`　**置信度**：high　**子系统**：gui-app
- **问题**：`app.rs` and `live_pipeline.rs` have zero unit or integration tests. The project rule requires 80%+ coverage. The pure computational paths `filter_block_with_chains`, `refilter_history`, and `build_filter_chains` are entirely untested, as is the CAR subtraction logic, the drop-detection arithmetic in `tick_device`, and the byte-count accumulator in Demo-mode recording.
- **影响**：Regressions in signal processing, byte-count reporting, and drop detection go undetected. The CAR and biquad filter code runs on every ingested block during live acquisition.
- **建议**：Add a `#[cfg(test)]` module to `app.rs` testing at minimum: `filter_block_with_chains` with CAR on/off, `refilter_history` roundtrip correctness, and the byte-count accumulator in `ingest_block` (Demo mode). Extract `export_kvraw` to its own module so it can be tested with a synthetic kvraw fixture.

#### `M28` [bug] PSD Hann-window normalization factor missing — power levels are not calibrated
- [ ] **定位**：`crates/kv-gui/src/fft_panel.rs:78-88`　**置信度**：high　**子系统**：gui-render
- **问题**：When a Hann window is applied, the PSD estimate must be corrected for the window's power loss. The standard correction is to divide by the squared sum of window coefficients (S2 = sum(w_i^2)) or equivalently multiply by 8/3 for the Hann window. The code divides only by (n * sample_rate), omitting the S2 correction. This makes absolute power levels ~4.6 dB too low compared to a calibrated periodogram, so displayed dB values are systematically wrong.
- **影响**：PSD magnitudes are mis-calibrated. A user comparing recorded spectra across sessions, or against a reference instrument, will see a consistent ~4.6 dB discrepancy. The shape (relative frequency content) is correct, but absolute power is wrong.
- **建议**：Compute the window normalization factor before the FFT: let s2: f64 = (0..n).map(|i| { let w = 0.5*(1.0-(pi2_over_n*i as f64).cos()); w*w }).sum(); Then use: let power = (real[k]*real[k] + imag[k]*imag[k]) / (s2 * sample_rate);

#### `M29` [project-rule] Hardcoded 0.195 µV/count ADC conversion factor in FFT violates hardware-independence rule
- [ ] **定位**：`crates/kv-gui/src/fft_panel.rs:72`　**置信度**：high　**子系统**：gui-render
- **问题**：The line real.push(sample as f64 * 0.195 * w) embeds the RHD chip's ADC gain constant directly in the FFT pipeline. Project rule 1 prohibits hardcoding ADC conversion factors before hardware confirmation, and requires that upper layers depend only on stable internal contracts (SampleBlock). The ring already normalizes to [-1,1] via i16::MAX; re-applying 0.195 here double-converts and ties the FFT µV calibration to a specific chip. Additionally, 0.195 is defined as RHD_FULL_SCALE_UV/i16::MAX in waveform.rs but is duplicated here as a magic literal.
- **影响**：FFT frequency display will show wrong µV values on any non-RHD hardware, and a future maintainer may not realize the factor is device-specific. Duplicating the constant risks divergence if the calibration is ever corrected in one place.
- **建议**：Reference the named constant: use crate::waveform::RHD_FULL_SCALE_UV and apply as sample as f64 / i16::MAX as f64 * RHD_FULL_SCALE_UV * w. Or, since the ring already stores normalized values, pass the µV scale from device metadata rather than hardcoding it. If the FFT is intended to work across devices, let the call site supply an optional µV-per-normalized-unit parameter.

#### `M30` [security] Unbounded BufRead::read_line allows memory exhaustion from remote clients · 初判 high
- [ ] **定位**：`crates/kv-gui/src/remote_api.rs:252-253`　**置信度**：high　**子系统**：gui-support
- **问题**：handle_client() uses BufReader::read_line into a growable String with no size limit. A single malicious (or buggy) client can send a line of arbitrary length, forcing the server thread to allocate unbounded memory until the process is OOM-killed. Combined with the 0.0.0.0 binding issue, this is remotely exploitable.
- **影响**：Remote memory exhaustion leading to OOM kill of the acquisition process. Even from localhost a runaway script can trigger this.
- **建议**：Wrap the reader with a size-limited variant or check line length after the read: if line.len() > 4096 { break; }. Alternatively implement a manual byte-by-byte read with a cap, or use a BufReader with a fixed-size internal buffer and an explicit take(4096) adapter before calling read_line.

#### `M31` [memory-safety] Missing SAFETY comment on unsafe block in diskspace.rs
- [ ] **定位**：`crates/kv-gui/src/diskspace.rs:38-46`　**置信度**：high　**子系统**：gui-support
- **问题**：The unsafe block calling GetDiskFreeSpaceExW has no // SAFETY: comment documenting the invariants that make the call sound. Per the project rules and Rust security guidelines, every unsafe block must have a SAFETY annotation.
- **影响**：No immediate safety risk here (the Windows API contract is met: the wide string is NUL-terminated and the output pointers are valid stack slots), but the missing annotation makes future code review harder and allows the invariants to be accidentally broken.
- **建议**：Add a // SAFETY: comment above the unsafe block, e.g.: // SAFETY: `wide` is a NUL-terminated UTF-16 Vec kept alive for the duration of the call; `free_to_caller` is a valid stack-allocated u64 written by the callee; the two null pointers are permitted by the Win32 API for unused output params.

#### `M32` [quality] panels.rs exceeds the 800-line project file-size limit
- [ ] **定位**：`crates/kv-gui/src/panels.rs:1-1201`　**置信度**：high　**子系统**：gui-support
- **问题**：panels.rs is 1,201 lines, which is 50% over the 800-line hard limit stated in the project coding rules. The file mixes display settings structs, filter settings structs, recording settings structs, device settings structs, multiple draw_ functions, and formatting helpers — low cohesion for a single file.
- **影响**：Reduced maintainability; violates an explicit project rule. The file is already growing and will continue to increase as new settings are added.
- **建议**：Split into focused modules: settings.rs (DisplaySettings, FilterSettings, RecordingSettings, DeviceSettings structs) and keep draw_* functions in panels.rs. The formatting helpers (format_time_window, format_uv, format_large_number) can move to theme.rs or a new format.rs.

#### `M33` [testing] IntegrityError::PacketIdWentBackwards is never exercised · 初判 high
- [ ] **定位**：`crates/kv-integrity/tests/integrity_report.rs:1-244`　**置信度**：high　**子系统**：tests
- **问题**：The integrity checker has two fatal error variants: `InvalidBlock` (tested) and `PacketIdWentBackwards` (not tested). The backwards-ID branch is reachable when packet IDs arrive out of order (e.g., after a device restart or ring buffer corruption), which is a plausible failure mode in both the RHD hardware and any future real-time path. Both `check_blocks` and `IncrementalIntegrity::push` implement this check; neither path is verified by a test.
- **影响**：A regression removing or inverting the backwards-ID guard would go undetected. Callers that rely on `Err(IntegrityError::PacketIdWentBackwards)` to stop recording on device reset would receive a spurious `Ok` report instead.
- **建议**：Add two tests to `crates/kv-integrity/tests/integrity_report.rs`: one for `check_blocks` and one for `IncrementalIntegrity::push`. Construct a two-block sequence where the second block has `packet_id = 0` after a first block with `packet_id = 5`, assert that both return `Err(IntegrityError::PacketIdWentBackwards { previous_packet_id: 5, observed_packet_id: 0 })`.

#### `M34` [testing] StreamingRecorder inconsistency checks for sample_rate, channel_count, and samples_per_channel are untested · 初判 high
- [ ] **定位**：`crates/kv-recorder/tests/recording_writer.rs:428-448`　**置信度**：high　**子系统**：tests
- **问题**：The `StreamingRecorder` validates consistency across blocks for four fields: `device_id`, `sample_rate`, `channel_count`, and `samples_per_channel`. Only the `device_id` mismatch path is covered (`streaming_recorder_rejects_inconsistent_device_id`). The other three fields are checked identically in the same code path but have no tests. A refactor that accidentally removes the sample_rate or channel_count check would not be caught.
- **影响**：A recording with mixed channel counts or sample rates would silently produce a malformed .kvraw file. Downstream analysis tools that read the header's declared `channel_count` would mis-interpret the interleaved sample layout.
- **建议**：Add three more tests to `recording_writer.rs`: `streaming_recorder_rejects_inconsistent_sample_rate`, `streaming_recorder_rejects_inconsistent_channel_count`, `streaming_recorder_rejects_inconsistent_samples_per_channel`. Each writes one valid block then a second block with the offending field changed, asserting the correct `InconsistentBlockConfig` variant and field name.

#### `M35` [testing] KvrawReader error paths (corrupt magic, truncated header, out-of-bounds reads) are not tested · 初判 high
- [ ] **定位**：`crates/kv-recorder/tests/recording_writer.rs:451-493`　**置信度**：high　**子系统**：tests
- **问题**：The `KvrawReader` only has a round-trip happy-path test (`kvraw_reader_round_trips_streaming_data`). No test verifies behavior when the file has a wrong magic (e.g., a raw .kvraw v1 file), a json_len field larger than KVRAW_JSON_RESERVED, a truncated header, or when `read_frames`/`read_channels` is called with an offset beyond the data region. The v1 fallback path (loading companion .json) is also not covered.
- **影响**：If a user opens a truncated or corrupt .kvraw file (e.g., from a crash during recording), the reader may panic on `read_exact`, silently return garbage data, or use wrong metadata from the fallback path — all without a failing test to alert the developer.
- **建议**：Add tests to `recording_writer.rs`: (1) open a file containing only a few bytes (truncated header) and assert `Err(RecorderError::Io)`, (2) open a file with a valid header but no sample data and assert `read_frames` returns an empty slice or appropriate error, (3) open a v1-style raw file (no KVRAW_MAGIC header) with a companion .json file and verify metadata is read from the JSON. These tests should use `fs::write` to construct synthetic files.

#### `M36` [testing] export_intan_rhd silently drops trailing samples and has no data-content verification test
- [ ] **定位**：`crates/kv-recorder/src/export_formats.rs:108-133`　**置信度**：high　**子系统**：tests
- **问题**：The Intan .rhd writer packs samples in fixed 128-sample data blocks. Samples that do not fill a complete final block are silently dropped (`while offset + RHD_SAMPLES_PER_BLOCK <= total_samples`). The existing test (`intan_rhd_creates_file`) only checks that the file exists and the magic bytes are correct. It does not verify the number of data blocks written, the amplifier data content, or what happens when the total sample count is not a multiple of 128.
- **影响**：Downstream tools reading the .rhd file will see fewer samples than were recorded, with no warning. An analysis pipeline computing mean firing rates or epoch-aligned averages will silently use truncated data. The data-content error would be invisible from the metadata JSON.
- **建议**：Add a test with a block count that produces a non-multiple-of-128 total (e.g., 2 channels, 100 samples/channel, 1 block = 100 total samples): assert that only 0 data blocks are written (since 100 < 128), that the file size equals the header size. Add a second test with 256 samples that asserts exactly 2 data blocks and verifies specific sample byte values at known offsets. Decide whether silent truncation is the intended contract and document it, or change the loop to write partial final blocks.

#### `M37` [testing] CI excludes kv-gui from build, test, and link — only a cross-target clippy 'check' covers it, so kv-gui never actually compiles to a binary in CI
- [ ] **定位**：`.github/workflows/ci.yml:22-38`　**置信度**：high　**子系统**：docs-build-ci
- **问题**：Both CI jobs run on ubuntu-latest. The test job runs `cargo test --workspace --exclude kv-gui` (kv-gui has zero automated test execution). The lint job runs clippy on the workspace excluding kv-gui, then `cargo clippy -p kv-gui --target x86_64-pc-windows-msvc -- -D warnings`. Clippy against the MSVC target does a metadata/type check without linking (no MSVC linker on Linux), so it catches type and borrow errors but never produces or links a kv-gui binary and never runs its dsp/playback/channel tests. Combined with the workflow_dispatch-only trigger, kv-gui effectively has no automated verification at all. The dev-handoff repeatedly notes 'GUI smoke test still pending on Windows' across many sessions, confirming kv-gui is verified only by hand. This is a defensible compromise (winit/eframe is Windows-first), but the gap should be tracked: a windows-latest job for `cargo test -p kv-gui` is the obvious fix and is even listed as a 'Next' item in Session 26.
- **影响**：Link errors, runtime panics, and kv-gui unit-test regressions (e.g. the 9 dsp tests, playback, channel_select, trigger tests) are not caught by CI. kv-gui is the largest crate by file count and the user-facing surface.
- **建议**：Add a windows-latest job that runs `cargo build -p kv-gui` and `cargo test -p kv-gui` (the tests are platform-agnostic dsp/logic tests). At minimum document in the handoff that kv-gui is hand-verified only, and keep the windows CI job on the backlog as already noted.

#### `M38` [security] Bundled proprietary okFrontPanel.dll committed with no license or attribution
- [ ] **定位**：`third_party/opalkelly/windows-x64/okFrontPanel.dll:1`　**置信度**：high　**子系统**：docs-build-ci
- **问题**：A 217,600-byte Opal Kelly FrontPanel runtime DLL is committed under third_party/opalkelly/windows-x64/. There is no LICENSE, NOTICE, README, or attribution file anywhere under third_party/ (`git ls-files third_party/` returns only the DLL). docs/15-dev-handoff.md line 1023 notes it was 'Copied from downloaded Open Ephys RHD plugin resources for local packaging convenience.' okFrontPanel is Opal Kelly proprietary software; redistributing it in a source repo without recording its license terms / redistribution rights is a licensing risk, and bundling a binary with no provenance record is a supply-chain/integrity concern (no checksum, no version pinned).
- **影响**：Potential license violation redistributing a vendor proprietary binary; no recorded provenance, version, or checksum for a binary that is loaded via FFI at runtime, making it impossible to verify integrity or audit the version.
- **建议**：Add a third_party/opalkelly/README or NOTICE recording the source, FrontPanel version, and redistribution terms (confirm Opal Kelly's redistribution policy). Consider documenting a checksum and whether the DLL should be vendored at all vs. required as a separately-installed runtime.

#### `M39` [quality] kv-cli default RHD bitfile path uses compile-time CARGO_MANIFEST_DIR with brittle ../../../.. traversal · 初判 low
- [ ] **定位**：`crates/kv-cli/src/lib.rs:1194-1201`　**置信度**：high　**子系统**：docs-build-ci
- **问题**：default_rhd_bitfile_path() builds the default bitfile location from `env!("CARGO_MANIFEST_DIR")` joined with four `..` segments plus `keyvast_260607_with_UART.bit`. CARGO_MANIFEST_DIR is fixed at compile time, so a shipped/installed kv-acq binary points at the build machine's source tree, not the runtime location — the same B4/B12 issue the dev-handoff (Session 21, lines 393-395) says was fixed for kv-gui by searching exe dir -> cwd -> fallback. kv-gui's default_bitfile_path (panels.rs:273) does the right thing (exe dir, then cwd, returns None when absent); kv-cli was not updated to match and still relies on a compile-time path with a magic `..` count tied to the crate's nesting depth.
- **影响**：A distributed kv-acq binary defaults to a bitfile path that only exists on the original build machine; moving the crate deeper/shallower in the tree silently breaks the `..` count. Inconsistent with the already-fixed kv-gui resolver.
- **建议**：Mirror kv-gui's runtime resolution (search current_exe() dir, then current_dir(), with a clear error when not found) instead of a compile-time CARGO_MANIFEST_DIR + fixed `..` traversal, and align the default filename with the reconciled bitfile decision.

#### `M40` [data-integrity] parse_custom_mapping does not reject duplicate channel indices
- [ ] **定位**：`crates/kv-gui/src/channel_map.rs:77-93`　**置信度**：medium　**子系统**：completeness-critic
- **问题**：parse_custom_mapping validates that each channel index is in-range but does not check for duplicates. A user-entered string like '0,0,1,2' produces a valid Vec<usize> with a repeated index. This mapping is then stored in display.channel_order and applied to every rendered frame. The result is that one physical channel occupies two display slots, the channel count shown in the UI is wrong, and any downstream consumer that uses the channel_order as indices into data arrays (e.g. filter_block_channels in the recorder selective-save path) will write duplicated channel data to disk, silently corrupting the recording.
- **影响**：Silent data duplication in selective-channel recordings when the user enters a custom map with repeated indices. The on-screen waveform display will also show the same channel twice, potentially confusing experimenters about actual channel count.
- **建议**：After building the order Vec, check for duplicates: `let mut seen = std::collections::HashSet::new(); for &ch in &order { if !seen.insert(ch) { return Err(format!("Duplicate channel {ch}")); } }` Add a test for this case.

#### `M41` [concurrency] live_pipeline recorder-command channel is unbounded — rapid Start/Stop cycles queue without limit
- [ ] **定位**：`crates/kv-gui/src/live_pipeline.rs:146`　**置信度**：medium　**子系统**：completeness-critic
- **问题**：The channel between the GUI and the recorder thread uses `mpsc::channel::<RecorderCmd>()`, which is unbounded. If the user (or a remote API script) sends many Start/Stop commands in rapid succession — or if the recorder thread is slow to wake because it is blocked on disk I/O — the command queue can grow without bound. Each RecorderCmd::Start carries a PathBuf and an Option<Vec<usize>>. In the remote API path the bounded check identified for the command/response queues does not apply here. The consequence is that previously opened recording files are never cleanly finalized until the commands are eventually drained, which may be never if Terminate arrives and the loop exits after processing only one message per condvar wake.
- **影响**：Memory growth proportional to the number of pending commands. More concretely: if Terminate arrives in the queue ahead of a Stop for an active recording, the recording is finalized by the Terminate arm but any pending Start commands after that are never processed, leaving dangling file paths that were never opened.
- **建议**：Use a bounded sync_channel with a small capacity (e.g. 4) matching the expected command cadence. The GUI should handle TrySendError::Full by showing an error toast rather than silently dropping the command. Alternatively, model the recorder state machine so only one outstanding command can be in flight at a time.

#### `M42` [bug] playback.rs tick() does not guard against NaN/infinity sample_rate from corrupt file metadata
- [ ] **定位**：`crates/kv-gui/src/playback.rs:161-162`　**置信度**：medium　**子系统**：completeness-critic
- **问题**：tick() computes `(dt * sample_rate * self.speed).round() as u64` without validating that sample_rate is finite and positive. A v1 .kvraw file whose header is corrupt or missing can produce sample_rate = 0.0 (which makes frames_to_advance = 0 forever, freezing playback silently) or sample_rate = NaN/infinity. Since Rust 1.45, float-to-integer casts use saturating semantics (NaN → 0, infinity → u64::MAX), so this is not undefined behaviour, but infinity * speed produces u64::MAX which when passed to saturating_add clamps cursor_frame to total_frames, immediately triggering auto-pause — the user sees playback start and instantly stop with no error message. The file is already opened (load_file succeeds as long as KvrawReader::open succeeds), so there is no prior validation gate.
- **影响**：Playback of a file with corrupt sample_rate metadata silently freezes or instantly auto-pauses. The user receives no diagnostic message pointing to the bad sample_rate field.
- **建议**：Validate sample_rate > 0.0 && sample_rate.is_finite() in load_file after extracting metadata, and set self.error / return early if invalid. Add the same guard at the top of tick() as a defensive check.

#### `M43` [performance] FanoutBlockBuffer::push allocates Arc on the acquisition hot path while holding the Mutex
- [ ] **定位**：`crates/kv-gui/src/live_pipeline.rs:256-259`　**置信度**：medium　**子系统**：completeness-critic
- **问题**：The producer thread calls `shared.0.lock().expect(...).push(block)` which calls `FanoutBlockBuffer::push()`. That function immediately does `Arc::new(block)` — a heap allocation — while the Mutex is held. This means every block acquisition involves: one clone() for the preview channel (line 247), one Arc::new heap allocation inside the lock (kv-buffer/src/lib.rs:105), and N Arc::clone calls for each consumer. The Arc::new allocation in particular holds the Mutex across a heap allocation, which under memory pressure can take an unbounded amount of time, stalling the acquisition thread.
- **影响**：Under memory pressure or allocator contention, the Mutex held during Arc::new can block the acquisition thread for an unbounded duration, causing preview frame drops and potential recorder buffer overflow. This violates the project's real-time constraint: 'hot paths must avoid locking that blocks the acquisition thread.'
- **建议**：Wrap the block in Arc before acquiring the lock: `let block = Arc::new(block); { let mut buf = shared.0.lock().expect(...); buf.push_arc(block); } shared.1.notify_one();` and add a push_arc method to FanoutBlockBuffer that accepts Arc<SampleBlock> directly, cloning the Arc for each consumer without re-allocating.

## 6. 🔵 Low（52）

> 紧凑列出；逐条均有 `文件:行号` 可定位。

- [ ] `L1` [design] **No workspace-level dependency table; shared external crates pinned per-crate** — `Cargo.toml:21-23`　The root workspace defines workspace.package but no workspace.dependencies table. log, env_logger, and windows-sys are pinned independently across kv-rhd, kv-gui, kv-cli, and kv-core, with windows-sys feature sets managed in two places. Nothing enforces version agreement.
- [ ] `L2` [docs] **Data-model doc drift: SampleBlock has four fields the documented contract omits** — `docs/04-data-model.md:9-21`　The data-model doc is the stated stable cross-crate contract, but its SampleBlock stops at the data field. The real struct in kv-types additionally carries aux_data, board_adc_data, ttl_in_per_sample, and ttl_out_per_sample, all optional. The nested aux layout indexed by stream then aux channel then sample is undocumented.
- [ ] `L3` [design] **RHD-to-generic config bridge hardcodes TTL line count and transport kind** — `crates/kv-rhd/src/protocol.rs:112-121`　The RhythmDataConfig device_config method converts RHD config into the generic DeviceConfig. It hardcodes ttl_line_count 16 and ttl_enabled true and pins the backend kind to Usb. DEFAULT_TTL_LINE_COUNT exists in kv-types but is unused here, and per rule 1 transport details are TBD until hardware confirmation.
- [ ] `L4` [data-integrity] **SampleBlock::validate() does not reject NaN or infinite sample_rate** — `crates/kv-types/src/lib.rs:87-89`　The guard `if self.sample_rate <= 0.0` passes for f64::NAN because NaN comparisons always return false, and also passes for f64::NEG_INFINITY. A block with sample_rate = f64::NAN will pass validate() and propagate into downstream calculations (e.g. timestamp_after_block, waveform scaling, recording metadata). The same issue applies to DeviceConfig::sample_rate, which has no validation at all.
- [ ] `L5` [performance] **FanoutBlockBuffer consumer lookup is O(n) linear scan while holding the Mutex** — `crates/kv-buffer/src/lib.rs:144-163`　Both consumer() and consumer_mut() scan the consumers Vec with .find(), iterating all registered consumers on every push(), pop(), and status() call. Every call to FanoutBlockBuffer::push() iterates all N consumers (line 107). In the current two-consumer case this is negligible, but the API is public and the consumers Vec is unbounded, so anyone adding more consumers degrades every hot-path push call proportionally. The push() call is made from the producer thread while holding the Mutex, so linear scan time directly extends the critical section.
- [ ] `L6` [performance] **drain_consumer clones every block out of Arc — defeats the purpose of Arc-sharing in FanoutBlockBuffer** — `crates/kv-core/src/pipeline.rs:256-259`　FanoutBlockBuffer wraps pushed blocks in Arc<SampleBlock> specifically to share them cheaply across consumers. However, drain_consumer immediately dereferences and clones each block back into an owned SampleBlock with `(*block).clone()`. This negates the Arc's purpose: every drained block still incurs a full deep copy of the Vec<i16> data payload. The run_threaded_pipeline result type holds Vec<SampleBlock>, so Arc cannot be used there end-to-end, but the pattern shows Arc sharing provides no benefit in this code path.
- [ ] `L7` [testing] **SampleBlock and FanoutBlockBuffer have no inline unit tests; test coverage is integration-only** — `crates/kv-types/src/lib.rs:1-245`　All tests for kv-types and kv-buffer live in the external tests/ directories. The kv-types crate has zero #[cfg(test)] mod tests blocks, meaning edge cases for validate() (NaN sample_rate, zero channels, overflow of expected_sample_values, etc.) and validate_against_ttl_lines() are not exhaustively covered. The kv-buffer crate similarly has no unit tests for the internal ConsumerQueue push/overflow logic in isolation. The existing integration tests are good but would not catch regressions in individual helper methods.
- [ ] `L8` [docs] **Public types and methods lack doc comments** — `crates/kv-buffer/src/lib.rs:8-262`　Virtually all public items in kv-buffer (BlockBuffer, FanoutBlockBuffer, BufferConsumerId, BufferStatus, ConsumerBufferStatus, FanoutBufferStatus, BufferError and their methods) have no /// documentation. In kv-types, DeviceConfig, SampleBlock (apart from its optional fields), AcquisitionState, DeviceStatus, and IntegritySummary are also undocumented. Both crates are internal shared-contract libraries that every upstream crate depends on; undocumented types make it harder to understand invariants (e.g. what does pushed_blocks mean when a block is dropped — is it counted before or after the drop?).
- [ ] `L9` [bug] **Plain u64 subtraction on CPU FILETIME values can panic in debug or silently wrap in release** — `crates/kv-core/src/process_metrics.rs:77`　The expression (end_kernel - self.start_kernel) + (end_user - self.start_user) uses plain Rust arithmetic. In debug mode this panics on underflow; in release mode it wraps silently. While GetProcessTimes is documented to only increase monotonically, calling finish() before meaningful CPU time has elapsed, or a race between two calls at process boundary, could yield end < start. The addition of the two deltas can also overflow u64 (extremely unlikely but structurally unchecked).
- [ ] `L10` [testing] **No test for recorder buffer overflow (slow consumer path) in run_threaded_pipeline** — `crates/kv-core/tests/threaded_pipeline.rs:1-140`　The test pipeline_preview_drops_without_affecting_recorder exercises preview drop with a capacity-2 preview buffer, but there is no test for the case where the recorder buffer overflows (recorder_capacity_blocks smaller than requested_blocks and the consumer drains slowly). The code in drain_consumer silently drops blocks in the ring buffer via BlockBuffer::push's eviction logic. Without a test covering this scenario, the claim in the doc-comment ('bounded memory usage') is not verified for the recorder consumer.
- [ ] `L11` [project-rule] **cargo fmt check fails for kv-cli (project-wide CI would block)** — `crates/kv-cli/src/lib.rs:1`　cargo fmt --check reports formatting diffs in crates/kv-cli/src/lib.rs (import grouping on line 1 and enum variant formatting starting around line 164). While this is outside the assigned kv-core subsystem, it means the project currently fails the formatting gate and CI would block all merges. The Rust coding-style rules require cargo fmt to be run before committing.
- [ ] `L12` [bug] **Infinite loop when u64::MAX is in drop_packet_ids** — `crates/kv-simulator/src/lib.rs:53-59`　The packet-skip loop uses `saturating_add(1)` to advance past dropped packet IDs. When `next_packet_id` reaches `u64::MAX` and `u64::MAX` is present in `drop_packet_ids`, `saturating_add(1)` returns `u64::MAX` again, `binary_search` finds it, and the loop body re-executes — indefinitely. A caller can reach this via any `drop_packet_ids` entry that equals `u64::MAX`, which is a valid `u64` value and imposes no compile-time restriction.
- [ ] `L13` [design] **SimulatorBackend does not implement AcquisitionSource — violates DeviceBackend contract rule** — `crates/kv-simulator/src/lib.rs:36-131`　The project rules in AGENTS.md state: 'upper layers depend only on stable internal contracts (SampleBlock, DeviceStatus, DeviceBackend trait)'. The acquisition contract trait in this codebase is `AcquisitionSource` (defined in kv-core). `SimulatorBackend` exposes only the inherent method `next_block()` and is consumed by wrapping it in ad-hoc closures (e.g., `move || sim.next_block().map_err(|e| e.to_string())`) everywhere it is used. This means the simulator is wired up outside the trait system, making it possible to silently diverge from the contract as the API evolves.
- [ ] `L14` [error-handling] **Default::default() calls expect() — panic in non-test code path** — `crates/kv-simulator/src/lib.rs:134-137`　`impl Default for SimulatorBackend` calls `Self::new(SimulatorConfig::default()).expect(...)`. While the default config is currently valid, `Default` is a public trait impl that downstream code can call at any time. If `SimulatorConfig::default()` or `DeviceConfig::simulator_default()` is ever changed such that the resulting config fails validation, this becomes a panic at the call site with no recoverable error path. Using `expect` in a `Default` impl is an anti-pattern for library code.
- [ ] `L15` [testing] **per-sample TTL fields always None — SampleBlock contract incompletely exercised** — `crates/kv-simulator/src/lib.rs:76-78`　`SampleBlock` defines `ttl_in_per_sample: Option<Vec<u32>>` and `ttl_out_per_sample: Option<Vec<u32>>` for per-sample TTL word streams. The simulator always sets these to `None`. No test verifies behavior of consumers (recorder, integrity checker, GUI) when these fields are populated. Because the simulator is the only synthetic source, any code path that reads `ttl_in_per_sample.as_ref().map(...)` is entirely untested.
- [ ] `L16` [testing] **test coverage gaps: no tests for TTL edge cases, spike activity, or large packet_id values** — `crates/kv-simulator/tests/simulator_backend.rs:1-123`　The five existing tests cover: (1) default block validity, (2) packet/timestamp advancement, (3) determinism across seeds, (4) packet drop gaps, and (5) invalid config rejection. Missing: (a) TTL bits are zero when `ttl_enabled = false` or `ttl_line_count = 0`; (b) TTL bits respect the `ttl_line_count` mask (no bits set above bit N-1); (c) spike values appear in generated data; (d) different channels produce different waveforms; (e) behavior near `next_packet_id = u64::MAX`.
- [ ] `L17` [error-handling] **set_sample_rate_30khz discards the bool that confirms the rate was accepted** — `crates/kv-rhd/src/backend.rs:414-417`　set_sample_rate() returns Ok(false) when the requested rate is not in its lookup table. set_sample_rate_30khz() calls it with ? — which propagates Err variants — but discards the Ok(bool), meaning a false return (rate not recognised) is silently ignored and the PLL is not actually programmed. This is unlikely to trigger for 30 kHz, but the pattern creates a latent bug if the lookup table is ever changed.
- [ ] `L18` [bug] **scan_ports_for_headstage: enabled_streams shift can overflow u32 for port 7 when enabled_streams > 4** — `crates/kv-rhd/src/backend.rs:903-910`　stream_bits = (1_u32 << enabled_streams) - 1 and the shift stream_bits << first_stream where first_stream = port * 4 (up to 28 for port 7). In Rust, left-shifting a u32 by >= 32 bits panics in debug mode and is UB in release mode. Currently MAX_SUPPORTED_STREAMS = 2, so stream_bits = 3 and the largest shift is 3 << 28 which fits. However, the scan function takes enabled_streams: usize from the caller, and the only guard is the upstream call from initialize_rhd_chips which passes the user-supplied value before the protocol.rs validator clamps it. If a caller passes enabled_streams > 4 and port = 7, first_stream = 28, stream_bits << 28 would shift bits off the top of u32.
- [ ] `L19` [bug] **register_value arithmetic can silently overflow u8 for register 0 and register 5** — `crates/kv-rhd/src/commands.rs:276-306`　Registers 0 and 5 are assembled with arithmetic on u8 values without wrapping/saturating: e.g. register 0: (adc_reference_bw << 6) + (amp_fast_settle << 5) + ... in Rust u8 arithmetic. If any field is out of its expected 1-2 bit range (which is possible because the struct fields are raw u8 with no invariants enforced), the addition wraps silently in release mode and panics in debug mode. For example, adc_comparator_bias = 3 (2 bits), shifted 2: 0b1100; adc_reference_bw = 3 (2 bits), shifted 6: 0b1100_0000; together that is 0xCC which is fine — but if adc_buffer_bias were somehow 64, register 1 would overflow.
- [ ] `L20` [quality] **Dead code stubs for set_dac_threshold, set_led, set_external_fast_settle_channel contain TODO bodies** — `crates/kv-rhd/src/backend.rs:1130-1148`　Three methods marked #[allow(dead_code)] return Ok(()) without implementing anything and carry TODO comments. There is no tracking issue or AGENTS.md note for these stubs. The project rule for TODO stubs is to document the specific register map reference; the current form gives no indication of what work remains or what FPGA version it targets.
- [ ] `L21` [memory-safety] **Read helpers panic on out-of-bounds access if length check is bypassed** — `crates/kv-rhd/src/parser.rs:193-222`　`read_u16_le`, `read_u32_le`, and `read_u64_le` index into `raw` directly without bounds checking. The upfront `raw.len() != expected_len` guard protects against a truncated buffer, but the guard relies on `bytes_per_block` computing the same byte count as the loop actually walks. Any future discrepancy between the formula and the loop (e.g. an off-by-one in filler words as documented in finding 1) will cause a panic inside the read helpers rather than returning a structured error. Hardware-supplied data is untrusted and should never be able to panic the process.
- [ ] `L22` [bug] **Timestamp rollover breaks intra-block continuity check** — `crates/kv-rhd/src/parser.rs:61-67`　The timestamp check uses `wrapping_add` to compute the expected value, which is correct for individual incrementing. However `first_timestamp` is stored as `u32` and the expected value is `first_timestamp.wrapping_add(sample_index as u32)`. When `first_timestamp` is near `u32::MAX` and the block spans the rollover boundary (samples cross the 0 point), the check will still work correctly. But there is no validation that consecutive *blocks* share a monotone relationship; the `packet_id` counter is a `u64` managed externally, while the FPGA timestamp is a `u32` that wraps every ~39 hours at 30 kHz. No test exercises this rollover case.
- [ ] `L23` [quality] **Missing `#[must_use]` on `compute_impedance` and `auto_select_scale`** — `crates/kv-rhd/src/impedance.rs:118`　`compute_impedance` returns a `(f64, f64)` tuple and `auto_select_scale` returns a `ZcheckScale`. Both are pure computation functions where ignoring the return value is always a bug. Neither is marked `#[must_use]`.
- [ ] `L24` [quality] **`cargo fmt --check` fails (CI would be red)** — `crates/kv-cli/src/lib.rs:1`　`cargo fmt --check` exits with code 1 due to formatting differences in `kv-cli/src/lib.rs`, `kv-cli/src/main.rs`, `kv-cli/tests/simulator_recording.rs`, `kv-gui/src/remote_api.rs`, and `kv-gui/src/spike_overlay.rs`. These are not in the assigned subsystem but would block any CI gating on `cargo fmt`.
- [ ] `L25` [testing] **No parser unit tests for the single-stream frame layout** — `crates/kv-rhd/tests/rhythm_parser.rs:1-157`　The existing parser tests cover streams=1 only through the timestamp-offset test and the CLI smoke test, both of which generate their fixture data using the same `enabled_streams % 4` filler formula as the production code. There is no round-trip test that verifies the sample values for streams=1 (analogous to the streams=2 test at line 34). This means the filler-formula bug for single-stream mode is completely invisible to the test suite.
- [ ] `L26` [performance] **Per-sample `write_all` loop in `StreamingRecorder::write_block` — unnecessary BufWriter overhead** — `crates/kv-recorder/src/lib.rs:664-671`　Each call to `write_block` issues `channel_count * samples_per_channel` individual `write_all(&[u8; 2])` calls to the `BufWriter`. For the default 64ch x 64 samples config, that is 4096 calls per block, each copying 2 bytes. While the BufWriter absorbs the cost in most cases, each call still pays the overhead of the BufWriter's internal capacity check and pointer arithmetic. A single bulk write of the entire block's bytes is both simpler and faster.
- [ ] `L27` [bug] **Hand-rolled JSON parser matches key names found inside string values** — `crates/kv-recorder/src/lib.rs:1172-1271`　Each `get_*` closure uses `json.find(&format!("\"{key}\""))` which searches the entire JSON string from position 0. If an earlier string value happens to contain text that matches `"key_name"`, the parser finds the occurrence inside the value rather than the actual key, and returns a wrong or zero result. For example, if `device_id` were `"written_samples"`, parsing the `written_samples` field would find position 14 (inside device_id's value) and extract an empty number, yielding 0. In practice current device IDs (`simulator-0`, `rhd-*`) do not contain field names, but the parser provides no structural guarantee.
- [ ] `L28` [quality] **`cargo fmt --check` fails — CI would be blocked** — `crates/kv-recorder/src/export_formats.rs:1-405`　`cargo fmt --check -p kv-recorder` reports formatting diffs in both `src/export_formats.rs` and `src/lib.rs`. Differences include line-length overruns on closure definitions, misaligned trailing comments in channel-header write block, and indentation in `get_optional_u64` and `get_bool` closures. This is a blocker for any CI gate that enforces `--check`.
- [ ] `L29` [quality] **`cargo fmt --check` fails (unrelated file, but CI is non-green)** — `crates/kv-cli/src/lib.rs:1`　`cargo fmt --check` reports formatting differences in `crates/kv-cli/src/lib.rs` (import order and enum variant layout). Although this is outside the integrity crate, the project rules require CI to be green before review can assume a clean baseline. The diff between `fmt --check` and the on-disk source is reproducible.
- [ ] `L30` [bug] **Index panic possible in raw RHD path when `blocks` and `block_bytes` yield an offset beyond checked bounds** — `crates/kv-cli/src/lib.rs:563-578`　The length check at line 563 uses `saturating_mul`, so if `block_bytes * options.blocks` overflows `usize` the check produces a smaller-than-real `expected_bytes`, and the raw slice `&raw[start..end]` can panic with an out-of-bounds index on a subsequent iteration even though the initial length guard appeared to pass. On 32-bit targets or with very large block counts this is exploitable.
- [ ] `L31` [testing] **No unit tests inside `lib.rs` — `civil_date_from_unix_days` and helper functions are untested directly** — `crates/kv-cli/src/lib.rs:1237-1251`　The bespoke Gregorian calendar implementation `civil_date_from_unix_days` has no `#[cfg(test)]` module and no direct unit tests. The only coverage comes from the single integration test `run_directory_name_uses_documented_timestamp_format`, which checks the Unix epoch but does not test leap-year edge cases (e.g. 2000-02-29, 2100-03-01) or month-boundary crossings.
- [ ] `L32` [bug] **Device-mode stop_recording does not set recording.state = Idle synchronously** — `crates/kv-gui/src/app.rs:888-896`　In `stop_recording()` for `AcqMode::Device`, only a `RecorderCmd::Stop` is sent; `self.recording.state` is left as `RecordingState::Recording`. It only transitions to `Idle` when `RecorderEvent::Stopped` later arrives (line 1244). In the gap the user can press R again, call stop_recording again, or the remote API can call stop_recording, each of which sends an extra `RecorderCmd::Stop` to the recorder thread. The recorder thread ignores a `Stop` when no recorder is active, so the redundant command is harmless, but the GUI continues to show 'STOP REC' and allows remote callers to receive 'not recording' while the state is still Recording. Additionally, `stop_all()` at line 412-439 calls neither `stop_recording()` nor does it wait for `RecorderEvent::Stopped` — it just sets `recording.state = Idle` at line 435 directly, so the 'state goes Idle via RecorderEvent::Stopped' comment in the Device branch of `stop_recording` is inconsistent: `stop_all` bypasses it.
- [ ] `L33` [bug] **unwrap() on live_pipeline inside tick_device panics if pipeline races to None** — `crates/kv-gui/src/app.rs:1219`　`tick_device()` starts with an early return on `self.live_pipeline.is_none()` (line 1207), then immediately calls `self.live_pipeline.as_mut().unwrap()` at line 1219. Because `tick_device` is called from `update()` on the GUI thread and no other code can set `live_pipeline = None` between those two lines in the same thread, the panic cannot happen during normal single-threaded operation. However, the early-return guard and the `.unwrap()` three lines later are fragile: any future refactor that calls an intermediate method that might set `live_pipeline = None` (as `process_recorder_events` already does for `SourceError` at line 1271) would cause a panic in the GUI thread, violating the project rule that GUI failure must not stop acquisition. The pattern also fires the `.unwrap()` path after events have already cleared `live_pipeline`, for instance if `SourceError` handling at line 1271 sets `self.live_pipeline = None` — but that processing happens *after* the borrow scope ends, so currently the unwrap has already succeeded. The invariant is maintained only by careful ordering, not by the type system.
- [ ] `L34` [performance] **Unnecessary clone of device_error String every frame** — `crates/kv-gui/src/app.rs:1893`　`if let Some(err) = self.device_error.clone()` clones the error String every frame it is displayed. The clone is needed only because the closure passed to `egui::TopBottomPanel::show` borrows `ui`, preventing a simultaneous borrow of `self.device_error`. The correct fix is to borrow rather than clone.
- [ ] `L35` [bug] **fft_radix2 power-of-two invariant enforced only by debug_assert — silently produces wrong output in release** — `crates/kv-gui/src/fft_panel.rs:96-97`　fft_radix2 requires both slices to have a power-of-two length, enforced only by debug_assert!(n.is_power_of_two() && imag.len() == n). In release builds this assert is compiled out. FftState::fft_size is a pub field; any code path (serialized state, future UI, test) that sets it to a non-power-of-two value will cause the butterfly loop to terminate at a wrong stage, producing a numerically incorrect spectrum without any error or panic.
- [ ] `L36` [performance] **FFT: last_n_samples unnecessarily round-trips through i16, losing precision and adding quantization error** — `crates/kv-gui/src/disp_ring.rs:186-190`　last_n_samples converts stored f32 values back to i16 via rounding, then compute_spectrum converts them back to f64 and multiplies by 0.195 µV/count. The round-trip through i16 introduces ±0.5 LSB (≈0.1 µV) quantization error on every sample and allocates a Vec<i16> that is immediately iterated and discarded. The method comment says 'de-normalized' — the original purpose may have been to match a legacy API — but the only caller is the FFT panel.
- [ ] `L37` [bug] **Spike-overlay time axis uses interpolation (total-1 divisor) instead of physical sample spacing** — `crates/kv-gui/src/spike_overlay.rs:337-343`　Each snippet's X axis is computed by linearly interpolating over [x_left, x_right] using the sample index: t_ms = x_left + (i / (total-1)) * (x_right - x_left). The window x_left = -pre_ms, x_right = post_ms is calculated from the stored pre/post_samples counts. However, the interpolation assumes all samples are spaced equally across (pre_ms + post_ms) milliseconds. This is correct IF the window was captured at the expected sample rate. But if sample_rate changes after capture (reconfigure rebuilds buffers), previously captured snippets retain their samples but the x_left/x_right are now computed with the new sample rate, stretching or compressing the displayed waveform in time. Additionally, using (total-1) as the denominator when total==1 produces NaN (i=0 gives 0/0 = NaN in IEEE 754).
- [ ] `L38` [bug] **snippets_for() panics when called with any ch on a zero-channel store** — `crates/kv-gui/src/spike_overlay.rs:285-287`　snippets_for clamps the channel index via ch.min(self.bufs.len().saturating_sub(1)). When self.bufs is empty, saturating_sub(1) returns 0, and ch.min(0) = 0, so self.bufs[0] is indexed on an empty Vec, causing an out-of-bounds panic. The draw_spike_overlay function guards channel_count() == 0, but snippets_for is a public method with no guard, and future callers (tests, new panes) can reach this path.
- [ ] `L39` [performance] **Zero-reference lines allocated per frame even when show_grid is false** — `crates/kv-gui/src/waveform.rs:286-303`　The per-channel zero-reference line code is guarded by if settings.show_grid, which is correct. However the spike threshold lines (lines 272-283) create a Vec<[f64;2]> per channel per frame via PlotPoints::from(vec![...]) unconditionally when filters.spike_threshold_enabled is true — these small allocations happen for every visible channel every frame.
- [ ] `L40` [testing] **Missing unit tests for DisplayRing decimation correctness and collect_channel alignment** — `crates/kv-gui/src/disp_ring.rs:1-272`　disp_ring.rs has zero unit tests despite implementing non-trivial logic: t0 alignment on first block, eviction during capacity overflow, stride2 alignment for jitter prevention, and edge cases in collect_channel (empty ring, window entirely before t0, window past end). The only test coverage is the dsp.rs biquad tests. A regression in push_block or collect_channel could silently produce stale or misaligned display data.
- [ ] `L41` [data-integrity] **Config JSON serializer omits escaping for display_mode and last_source fields** — `crates/kv-gui/src/config_persist.rs:155, 171`　to_json() escapes output_dir (backslash and quote) and file_prefix (quote), but the display_mode and last_source string fields are interpolated directly into the JSON template without any escaping. Although these fields are currently only set to known-good literals ("sweep", "roll", "demo", "device", "playback"), they are loaded back from disk via extract_string which does handle unescape — so the serializer's inconsistency means a corrupted or hand-edited config file with a quote or backslash in display_mode will produce malformed JSON that silently falls back to defaults on the next load, destroying the user's setting.
- [ ] `L42` [data-integrity] **Config file save is not atomic — power-fail can corrupt the config** — `crates/kv-gui/src/config_persist.rs:279-281`　save_config() calls fs::write() which on Windows is not atomic — a crash or power failure mid-write produces a truncated or partial file. On the next startup, from_json() will parse a partial document; because the parser is lenient (missing fields use defaults), the user may silently lose all saved settings and not notice.
- [ ] `L43` [bug] **channel_spacing loaded from config is not clamped to valid range** — `crates/kv-gui/src/config_persist.rs:254`　apply_to() writes channel_spacing directly from the config without clamping to [SPACING_MIN, SPACING_MAX] (1.0..=6.0). An out-of-range value (e.g. 0.0 or 100.0 from a hand-edited config) is accepted silently and passed to the waveform renderer.
- [ ] `L44` [quality] **Redundant set_nonblocking(false) call in server_loop ignores error** — `crates/kv-gui/src/remote_api.rs:205-206`　start_server() already calls set_nonblocking(false) and returns an error if it fails. server_loop() repeats the same call on the moved listener and silently ignores the result with .ok(). The duplication is harmless in practice but is dead/misleading code.
- [ ] `L45` [testing] **kv-gui crate has no integration tests; live acquisition and waveform paths have zero coverage** — `crates/kv-gui/src/live_pipeline.rs:1-400`　The kv-gui crate is excluded from CI (per project notes) and has no integration test files. While several inline `#[test]` modules exist for pure-computation helpers (DSP filters, FFT, trigger logic, channel maps, remote API parsing), the critical acquisition-facing modules have zero coverage: `live_pipeline.rs` (the real-time bridge between the acquisition thread and GUI rendering), `disp_ring.rs` (the ring buffer for display data), `waveform.rs` (sweep and roll rendering), `playback.rs`, and `preview.rs`. The single ignored doc-test in `toast.rs` is the only doc-test in the crate.
- [ ] `L46` [testing] **FanoutBlockBuffer UnknownConsumer error path from pop() is not tested** — `crates/kv-buffer/tests/block_buffer.rs:145-153`　The `FanoutBlockBuffer::pop(consumer_id)` and `consumer_status(consumer_id)` methods return `Err(BufferError::UnknownConsumer)` when an invalid ID is used. The only test for `UnknownConsumer` is `fanout_rejects_zero_capacity_consumer`, which actually tests `add_consumer`, not `pop`. A caller holding a stale `BufferConsumerId` (e.g., after a buffer is rebuilt) would hit this path.
- [ ] `L47` [security] **Diagnostic scripts hardcode a local user/desktop path leaking the username and machine layout** — `compare_kvraw_vs_oe.py:21`　compare_kvraw_vs_oe.py hardcodes `DEFAULT_OE = r"C:\Users\Admin\Desktop\keyvast\2026-06-14_23-06-20"` as the default Open Ephys reference directory, and diag_hf.py, diag_rails.py, and diag_plot.py all import and use this DEFAULT_OE when their optional second argument is omitted. This is a machine-specific absolute path embedding the Windows username 'Admin' and a private desktop directory structure. If these scripts are committed (see the gitignore-gap finding), they leak that local environment detail and break for any other user. The scripts also depend on numpy/scipy/matplotlib with no requirements.txt or dependency declaration anywhere in the repo.
- [ ] `L48` [docs] **Inconsistent and stale FPGA bitfile references across docs and a hardcoded default in kv-cli** — `docs/12-confirmed-decisions.md:35`　docs/12-confirmed-decisions.md line 35 states 'First hardware bit file: D:\11111\1case\104_keyvast_gui\keyvast_260607_with_UART.bit' (an absolute path under a *different* project folder, 104_keyvast_gui, not this repo's 51_keyvast_gui). docs/14-open-questions.md line 309 and docs/15-dev-handoff.md (Session 16/17 lines 603, 622, 1027) repeat this same path. But docs/18-rhd-signal-debug.md (lines 28-36, 125) — the most recent debug log — explicitly says the bitfile to USE is `intan_rec_controller_7310.bit` (the file actually present in the repo root) and lists keyvast_260607_with_UART.bit as just one of several. Code is also inconsistent: crates/kv-cli/src/lib.rs:1200 hardcodes the default RHD bitfile name as `keyvast_260607_with_UART.bit` (and the test simulator_recording.rs:562 asserts that name), while crates/kv-gui/src/panels.rs:273-278 prefers `keyvast_combined_download.bit` then `keyvast_260607_with_UART.bit` then `intan_rec_controller_7310.bit`. Three different 'default' bitfile stories across kv-cli, kv-gui, and the docs.
- [ ] `L49` [design] **.cargo/config.toml hardcodes a single China mirror as the only crates.io source for every checkout** — `.cargo/config.toml:10-14`　The committed .cargo/config.toml replaces crates-io entirely with the Tsinghua (TUNA) sparse mirror for every checkout of this repo. CI works around it by deleting the file (ci.yml lines 19, 29: `rm -f .cargo/config.toml`), and the comments do list rsproxy.cn / ustc as swap candidates, so this is handled. However, committing a China-specific registry override into the repo means any contributor outside that network silently routes all crates.io traffic through a Tsinghua mirror (a privacy/availability coupling) unless they know to remove the file. This is a region-specific dev convenience leaking into shared config. The same pattern is duplicated in gui.bat / gui-log.bat as CARGO_* env vars.
- [ ] `L50` [bug] **demo.rs DemoGenerator next_block uses plain arithmetic additions that panic in debug on u64 overflow** — `crates/kv-gui/src/demo.rs:131-133`　DemoGenerator::next_block() increments self.global_sample and self.packet_id with plain `+=` arithmetic. In debug builds, integer overflow panics; in release builds it wraps silently. At 30 kHz / 64 spp the packet_id would require ~614 billion years to overflow u64, so this is not a practical concern for those fields. However, self.global_sample += spc as u64 where spc is read from a user-controlled SimulatorConfig means a pathologically large samples_per_packet (e.g., usize::MAX cast to u64) causes an immediate overflow on the first call.
- [ ] `L51` [testing] **channel_map.rs and playback.rs were entirely unreviewed — no test coverage for playback tick edge cases** — `crates/kv-gui/src/playback.rs:149-194`　The PlaybackManager::tick() function and its seek/cursor arithmetic have no unit tests. The files crates/kv-gui/src/playback.rs, demo.rs, preview.rs, channel_map.rs, and impedance_panel.rs were never assigned to a review subsystem, and the tests subsystem review confirmed that the entire kv-gui crate has no integration tests. The tick() path covers: state-machine transitions, floating-point cursor advancement, block_frames computation, read_block_at delegation, and last_emitted_frame deduplication — none of which are covered.
- [ ] `L52` [performance] **multiview.rs clones DisplaySettings on every render frame for every band-view tile** — `crates/kv-gui/src/multiview.rs:536-537`　draw_band_view (called for LfpView and ApView tiles) calls `let mut tile_display = self.display.clone()` on every egui frame — typically 60 times per second. DisplaySettings contains String fields (display_mode: String) and Vec<usize> (channel_order). These heap-allocated fields are cloned even though only the visible_channels field is subsequently mutated. With multiple tiles open, this allocates multiple String and Vec<usize> copies per frame.

## 7. ⚪ Info（7）

- [ ] `I1` [memory-safety] **c_long used for buffer length in read_from_block_pipe_out is 32 bits on Windows — limits transfer to 2 GB but not the real risk** — `crates/kv-rhd/src/frontpanel.rs:43, 219`　The read_from_block_pipe_out FFI signature uses c_long for the buffer length parameter and return value. On Windows x64, c_long is 32 bits (unlike Linux where it is 64 bits). buffer.len() is cast as c_long: if buffer.len() > i32::MAX (2 GB), this silently truncates on Windows. For current block sizes this is not reachable (block_bytes is at most a few hundred KB), but the cast has no assertion documenting the assumption.
- [ ] `I2` [performance] **`written_samples(blocks)` iterated twice in `write_recording_with_backend`** — `crates/kv-recorder/src/lib.rs:146-147`　`written_samples(blocks)` sums `block.data.len()` across all blocks. It is called twice in adjacent lines to populate `written_samples` and `byte_count` in `RecordingSummary`, causing two full O(n) iterations over the block slice.
- [ ] `I3` [error-handling] **No validation that `--channels 0` or `--sample-rate 0` is rejected early** — `crates/kv-cli/src/lib.rs:470-538`　`parse_benchmark_args` accepts `--channels 0` and `--sample-rate 0.0` without error. The validation inside `blocks_for_duration` silently returns 0 blocks, which is then caught by the `block_count == 0` guard, but the error message (`InvalidBlockCount { blocks: 0 }`) does not tell the user that the root cause was zero channels or sample-rate.
- [ ] `I4` [performance] **config_persist::load_or_default called twice on the first frame** — `crates/kv-gui/src/app.rs:1490`　`update()` calls `config_persist::load_or_default()` on the first frame (line 1490) to restore the window size, even though `KvApp::new()` already called it at line 224 and applied all settings. This means the config file is read from disk twice at startup.
- [ ] `I5` [performance] **DisplaySettings::clone() per frame in draw_band_view allocates on every render call** — `crates/kv-gui/src/multiview.rs:536-537`　draw_band_view calls self.display.clone() every frame to produce a tile-local DisplaySettings with visible_channels overridden. DisplaySettings likely contains several fields (amp_scale_idx, time_scale_idx, channel enable flags, colour maps, etc.). Cloning it per frame at 60 fps is unnecessary heap activity on the render hot path.
- [ ] `I6` [performance] **filter_block_channels allocates a new Vec inside the channel-select hot path** — `crates/kv-gui/src/channel_select.rs:117-132`　filter_block_channels() is called on the recording path when a channel subset is selected. It allocates a Vec<usize> (valid channels) and a Vec<i16> (filtered data) on every call. At 30 kHz with 64 channels the data Vec allocation is unavoidable (new shape), but the valid Vec is an intermediate that could be avoided by iterating indices lazily.
- [ ] `I7` [testing] **Rhythm parser does not test multi-sample-block with magic corruption in a non-first frame** — `crates/kv-rhd/tests/rhythm_parser.rs:59-69`　The `rejects_bad_magic` test corrupts only frame 0 of a 1-sample block. The parser loops over all samples and checks magic at every frame; there is no test that verifies corruption at sample index > 0 is caught with the correct `sample_index` field in the error.

## 8. 真实优点（诚实记录）

- crate 架构干净无环：kv-types 是规范的契约叶子层，依赖全部向下，SampleBlock 是唯一跨层契约——硬件无关性在结构上基本达成。
- fanout 缓冲内部用 Arc<SampleBlock> 避免每消费者拷贝，正确隔离 recorder 与 preview/GUI 消费者。
- recorder KVRAW v2 格式设计良好：头部占位清零、finish 时回填；崩溃（非正常停止）的录音可在重新打开时被正确检测。
- kv-rhd 体现真实硬件工程深度：MISO 延迟扫描、重试逻辑、半量程 0x4000 校验门控、模拟前端初始化均经审慎推理并对照 Open Ephys 参考行为。
- DSP 核心（RBJ Cookbook biquad）与阻抗 DFT 数学是忠实正确的移植，纯计算路径有扎实测试。
- 错误类型全程 typed 且有恰当 Display/Error 实现，避免 Box<dyn Error> 反模式；CLI 数值解析安全。
- 测试 100% 通过（非 GUI 工作区实测 103 个；含 GUI 内联单测全库共百余个）；生产代码在 clippy -D warnings 下干净（GUI 测试代码的 4 个 lint 见基线表 / H、CI 项）。
- 文档异常详尽且大体与实现（KVRAW 头布局、ADC 换算、CLI 表面）吻合；git 历史中并无实际提交的密钥或大二进制文件。

## 9. 被驳回的发现（3）— 验证有效性佐证

> 对抗式验证驳回了以下 3 条似是而非的发现，证明流程未放水。保留供参考，避免日后重复『发现』（理由保留英文原文）。

### ~~Blocks from different streams or devices are silently compared against each other~~　`crates/kv-integrity/src/lib.rs:62-106`
The claim asserts that `check_blocks` and `IncrementalIntegrity::push` blindly compare blocks from different `(device_id, stream_id)` pairs, which multi-stream hardware would interleave, causing false gap and backwards-ID reports. After reading every relevant file, the claim does not hold in this codebase.

**The cited code exists and matches the description (lines 62-106, 193-267 in `crates/kv-integrity/src/lib.rs`).** There is no `stream_id` guard in either function — that part is accurate. But the rest of the claim's chain of causation fails on a critical architectural fact.

**The RHD backend does NOT produce interleaved per-stream blocks.**

In `crates/kv-rhd/src/protocol.rs`, `RhythmDataConfig` holds a single `stream_id: u32` and an `enabled_streams: usize` count (max 2, enforced by `MAX_SUPPORTED_STREAMS = 2`). In `crates/kv-rhd/src/parser.rs`, `parse_rhythm_data_block` produces exactly ONE `SampleBlock` per USB read, flattening all enabled streams into a single `data` Vec (all streams are interleaved at the channel level inside one block):

```rust
// parser.rs lines 107-121
let block = SampleBlock {
    device_id: config.device_id.clone(),
    stream_id: config.stream_id,   // always config.stream_id (default 0)
    packet_id,
    ...
    channel_count,   // = enabled_streams * CHANNELS_PER_STREAM
    data,            // flat: all streams merged into one sample vector
    ...
};
```

And in `crates/kv-rhd/src/backend.rs`, `RhdHardwareBackend::read_block()` calls `parse_rhythm_data_block` once and returns a single `SampleBlock`. The `AcquisitionSource` trait (`crates/kv-core/src/lib.rs`, line 14) has exactly one method `read_block() -> Result<SampleBlock, ...>` — it returns one block per call, always with the same `(device_id, stream_id)` pair from the one `RhythmDataConfig`.

The pipeline callers (`run_fixed_blocks` in `lib.rs` line 204, `run_threaded_pipeline` in `pipeline.rs` line 207, `run_streaming_pipeline` in `pipeline.rs` line 384) each own a single `AcquisitionSource` and feed its successive blocks — which are homogeneous by construction — into the integrity checker. There is no path in the current code that would mix blocks from two different `(device_id, stream_id)` pairs into the same `check_blocks` call or `IncrementalIntegrity` instance.

**The "multi-stream" terminology is a false analogy.** Multiple enabled RHD streams (i.e. multiple SPI ports/headstages) are merged into a single wider `SampleBlock` by the parser, not emitted as separate blocks. The integrity functions therefore always see a homogeneous sequence.

**The claim is a plausible design concern for future multi-device work**, but it does not represent a real bug in the current code. The absence of a doc comment stating the single-stream requirement is a minor quality nit, not a high-severity bug. Corrected severity: low (documentation gap, not a real defect).

### ~~`block.data.len() as u64` is a potentially-truncating cast on 32-bit targets~~　`crates/kv-integrity/src/lib.rs:83`
The cited code exists exactly as described. Line 83 of `crates/kv-integrity/src/lib.rs` reads:

```rust
report.summary.written_samples = report
    .summary
    .written_samples
    .saturating_add(block.data.len() as u64);
```

The identical pattern appears at line 207 in `IncrementalIntegrity::push`. Both occurrences are real.

However, the claim's core technical assertion is wrong: `usize as u64` is NEVER a truncating cast on any Rust target, including 32-bit hosts.

On a 32-bit target, `usize` is 32 bits wide (max value `u32::MAX` = 4_294_967_295). Casting a 32-bit `usize` to `u64` is a zero-extending widening operation — `u32::MAX as u64` == `4_294_967_295u64`, which fits in `u64` with no information loss. Truncation requires casting from a wider type to a narrower type (e.g., `u64 as u32`). The direction here is the safe direction on every target Rust supports.

The reviewer's scenario — "if a block holds > 4 GiB of samples, the cast silently truncates on 32-bit" — is self-contradictory. A `Vec<i16>` on a 32-bit host cannot hold more than `usize::MAX` (~4B) elements because `Vec::len()` returns `usize`. So the maximum value `block.data.len()` can ever return on a 32-bit host is `u32::MAX`, and `u32::MAX as u64` widens correctly to `4_294_967_295u64`. The hypothetical > 4 GiB block is impossible to construct on a 32-bit host in the first place.

The `SampleBlock.data` field (`Vec<i16>`, defined in `crates/kv-types/src/lib.rs` line 56) is bounded in practice by `channel_count * samples_per_channel` (validated in `validate()` at lines 86–105 of kv-types). With the project's MVP constants (64 channels × 64 samples/packet = 4096 `i16` values per block), the actual length is tiny, making any platform concern entirely moot.

The cast is correct on all current and future Rust targets. This is a non-issue.

### ~~extract_string parser does not handle escaped backslashes at end of string~~　`crates/kv-gui/src/config_persist.rs:373-375`
The cited code exists exactly at lines 363-376 of crates/kv-gui/src/config_persist.rs:

```rust
let after = &after[1..]; // skip opening quote
let end = after.find('"')?;
Some(after[..end].replace("\\\\", "\\").replace("\\\"", "\""))
```

The serializer at line 165 is:
```rust
output_dir = self.output_dir.replace('\\', "\\\\").replace('"', "\\\""),
```

The reviewer's claimed failure scenario is: a Windows path ending in `\` (e.g. `C:\data\`) serializes to `C:\\data\\` in JSON, and then `after.find('"')` stops at the wrong position. This is incorrect.

The JSON byte sequence for `C:\data\` is: `C`, `:`, `\`, `\`, `d`, `a`, `t`, `a`, `\`, `\`, `"` (closing quote). The two-character sequence `\\` in the JSON bytes is two backslash characters — there is no `"` character embedded within it. The naive `find('"')` therefore correctly finds the real closing quote and the roundtrip is exact: `C:\\data\\` → `.replace("\\\\", "\\")` → `C:\data\`. No truncation occurs.

The only case where `find('"')` would stop prematurely is if the original string contained a literal double-quote character, because the serializer escapes it to `\"` (backslash + quote in the JSON bytes), and the `"` portion of that sequence would be found first. Windows paths cannot contain `"` (illegal in Windows filenames), so this cannot occur for `output_dir`. It could theoretically occur for `file_prefix` if a user typed a `"` into the file-prefix field, but that is a different, lower-severity concern not asserted by the claim.

The reviewer conflated `\\` (escaped backslash — no `"` involved) with `\"` (escaped quote — `"` present in the byte stream). The specific scenario described — truncation of `output_dir` for paths ending in backslash — cannot occur. The existing roundtrip test with `"C:\\data\\recordings"` passes not because it avoids the edge case, but because the edge case does not exist as described.

## 10. 优先级行动清单

### P0 — 修复后才能信任录制数据
- [ ] 把所有磁盘 I/O 移出共享 pipeline 互斥锁：锁内只收集 Arc<SampleBlock> 指针到局部 Vec，释放锁后再在临界区外写盘/克隆（pipeline.rs:319-324、179-183）；对 FanoutBlockBuffer::push 同样先 Arc 包装再取锁。
  - _理由_：消除首要的『采集卡顿+静默丢块』失效模式，直接落实实时铁律；对数据完整性影响最大。
- [ ] 让 BufferOverflow 可观测：push() 返回丢弃信息，由 pipeline/fanout 发出带 dropped_blocks 与 occupancy 的 AcquisitionEvent::BufferOverflow，并向 pipeline 暴露 dropped_blocks_total 计数。
  - _理由_：把静默的实时丢数据变为可记录、可监控的事件——正是项目规则要求、且事件类型已存在。
- [ ] 重写 export_intan_rhd 与 GUI export_kvraw 为逐块流式（内存上限 O(128*channels)），尾部不足 128 的 RHD 块改为补零或报错而非静默丢弃；增加帧数非 128 倍数的测试。
  - _理由_：同时修复任何真实导出的 OOM 崩溃与每段默认配置录音的静默截断——同一路径上两个独立的数据完整性缺陷。
- [ ] kv-integrity 的 packet_id 连续性改用 wrapping 算术（expected=previous.wrapping_add(1)；gap=current.wrapping_sub(expected)），并加 u64::MAX 到 0 回绕测试及多流、大间隔测试。
  - _理由_：防止计数回绕时假致命错误杀掉流式 pipeline 并截断录制；当前无任何测试覆盖。
- [ ] 修正 FFT 有效采样率（传 ring.sample_rate/ring.dwnsp）、补 Hann 窗归一化，并修 spike 不应期（waveform.rs:684-693）与 hover 幅值增益项（waveform.rs:457-460），使其使用抽取后采样率与用户 amp_scale。
  - _理由_：实时视图是操作者唯一的在线信号质量检查；这 4 个数值 bug 使显示的频率、功率、spike 计数、幅值全错，导致错误的实验判断。
- [ ] 传播硬件错误而非吞掉：wait_for_dcm_done/wait_for_data_clock_locked 返回 Result（新增 ClockNotLocked 变体）并在超时时中止 init；flush_fifo 返回 Result（容忍并记录读错误、上报 wire-in 失败）；检查 set_sample_rate_30khz 的 bool。
  - _理由_：阻止 PLL 或 FIFO 失败后采集在错误采样率下静默录制——那是对整段会话不可恢复的破坏。
- [ ] 远程 API 默认绑定 127.0.0.1 + 共享密钥 token，外部访问需显式开启；限制命令/响应队列、把 read_line 上限定到约 4KB。
  - _理由_：封堵未认证远程控制/DoS/数据完整性漏洞及相关远程 OOM 向量。

### P1 — 健壮性 / 可测性 / CI
- [ ] 修 RemoteApiHandle::stop() 存储并连接实际绑定端口；把 live_pipeline 的 unwrap（app.rs:1219）改为 let-else；对配置加载的 notch_idx、channel_spacing 做边界检查；snippets_for() 防零通道。
  - _理由_：消除违反铁律 4（GUI 失败不得拖垮采集/阻塞收尾）的 GUI 线程挂起与 panic。
- [ ] 给每个 unsafe 块（frontpanel.rs、process_metrics.rs、diskspace.rs）加 SAFETY 注释，记录库生命周期、句柄非空/独占所有权、缓冲有效性、NUL 终止；加 c_long debug_assert；把 parser 读 helper 改为返回 Option/Result 而非 panic。
  - _理由_：记录并加固最高风险的 FFI/unsafe 面，使日后重构不致静默引入 UB，并把不可信硬件输入引发的 panic 变为可恢复错误。
- [ ] 为 RHD probe/帧分析 helper（backend.rs:1461-1675）、RhdChipType::from_register63 分发、KvrawReader 错误路径、StreamingRecorder 一致性检查、PacketIdWentBackwards 补单测；启用运行 cargo build -p kv-gui 与 cargo test -p kv-gui 的 Windows CI job。
  - _理由_：把测试覆盖带到最高风险且当前未测的代码，使硬件 bring-up 回归与 reader/recorder 损坏在 CI 而非现场会话中被发现。
- [ ] 恢复自动 CI 触发（on: [push, pull_request]），以 build+test+clippy+fmt 门禁覆盖全部 crate（含 kv-gui）；对 workspace 跑 cargo fmt 使分支变绿。
  - _理由_：没有自动门禁，上述所有修复都可能静默回归；分支当前 fmt 失败且从不自动构建。
- [ ] 在 kv-cli 安装 Ctrl-C 处理器，置 AtomicBool 取消标志并穿入采集循环，使 recorder 在退出前刷盘、.kvraw footer 收尾。
  - _理由_：防止以正常方式停止长采集时丢失缓冲样本并留下不可读录音。

### P2 — 硬件无关性 / 仓库卫生 / 结构
- [ ] 用 SampleBlock/DeviceStatus 携带的命名常量或运行时配置替换泄漏的硬件常量：0.195uV/count（fft_panel.rs:72）、bitfile 名/电缆长/device-id（kv-cli）、DAC 1.225V 与 uV/count（impedance.rs）、TTL 线数/传输（protocol.rs）。
  - _理由_：恢复硬件无关性，防止非 RHD 后端或改板配置下读数错误；对当前单板 MVP 非阻塞，但接入第二后端前必须完成。
- [ ] 收紧 .gitignore（*.bit、__pycache__/、captures/、*.png、kvlog.txt、诊断 .py），为 third_party/opalkelly 加含 DLL 版本/出处/校验和的 NOTICE，把中国 crates 镜像移到 gitignore 的本地示例，去掉诊断脚本中的硬编码本机路径。
  - _理由_：防止误提交专有 2.6MB 二进制、规避 license/出处风险，并移除对协作者不友好、会泄露信息的仓库默认设置。
- [ ] 拆分四个超限文件（app.rs 2403、backend.rs 1676、kv-cli/lib.rs 1288、panels.rs 1201）为聚焦模块，抽取单一共享 FrameLayout helper 以消除六处重复的帧算术；引入真正的 DeviceBackend trait（或在文档中明确 AcquisitionSource 即契约）并让 SimulatorBackend 实现它。
  - _理由_：降低改动爆炸半径与评审难度，消除易致静默损坏的重复，并为未来硬件提供编译期可强制的后端契约。

### P3 — 长尾正确性与质量
- [ ] 处理其余中/低危正确性项：integrity 时间戳不连续双计（lib.rs:136-153）与 O(n*g) 间隔估计；SampleBlock::validate 的 NaN/无穷采样率；process_metrics/simulator/demo 的饱和算术以避免 debug panic/release 回绕；config 原子保存（temp 文件+rename）；write_latencies_us 加蓄水池上限；recorder 的正确 JSON 转义/解析。
  - _理由_：提升报告准确性、健壮性与长跑内存表现；单项影响低但整体把系统抬到生产质量。

---

## 附录 · 各子系统发现分布

| 子系统 | 发现数 |
|---|---|
| gui-render | 12 |
| rhd-hardware | 11 |
| gui-support | 11 |
| cli | 10 |
| gui-app | 10 |
| completeness-critic | 10 |
| recorder | 9 |
| tests | 9 |
| rhd-parsing | 8 |
| docs-build-ci | 8 |
| simulator | 8 |
| core-pipeline | 7 |
| types-buffer | 7 |
| architecture | 5 |
| integrity | 5 |

_本清单由多智能体审查 + 人工复核生成；如逐项修复，请在对应勾选框打勾并在 PR 中引用编号（如 `fix C1, H4`）。_