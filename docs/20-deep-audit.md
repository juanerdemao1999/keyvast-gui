# 20 · 深度审计：在体电生理生产化雷点清单（doc 19 之外）

> 审计日期：2026-06-25　|　分支：`devin/1781369404-gui-ux-overhaul`（当前工作区，含最新 Open Ephys 对齐提交）
>
> **场景**：团队自研硬件，已用 Open Ephys RHD 插件 + 自研 bitfile 跑通（Open Ephys/Intan 为黄金参考）；目标是用本 Rust 工程做自研在体电生理记录程序。
> 本清单**只收录 doc 19（130 条）之外**的新雷点，聚焦：实时丢数据、记录/导出文件损坏、科学信号失真、与 Open Ephys/Intan 协议不一致、崩溃毁记录、在体必需能力缺失。

## 0. 方法与状态图例

14 个透镜并行深挖 → 每条候选对抗式核验（High/Critical 双验证者：反驳者 + 去重/相关性）→ 跨透镜按根因合并。GUI 部分因核验阶段触发会话限流，已由人工逐条对源码复核。

| 状态 | 含义 |
|---|---|
| ✅ 已验证 | workflow 对抗验证确认，或（GUI/标注处）人工对源码复核确认 |
| ⏳ 待核实 | Find 阶段发现，验证者因限流未跑完，需人工复核（机制已附 file:line） |
| ⚠️ 部分争议 | 验证者意见分歧，已按保守口径收录并标注 |

## 1. 汇总

| 严重度 | 数量 |
|---|---|
| 🔴 Critical | 2 |
| 🟠 High | 21 |
| 🟡 Medium | 15 |
| 🔵 Low | 6 |
| **合计** | **44** |

> 另有 9 条与 doc 19 重复、8 条经对抗验证被驳回，见文末附录 A/B。

> **最该先处理（在体致命）**：`DA1` 数字/辅助通道全链路丢失（无法做刺激对齐）、`DA2` 一次瞬态即中止整段会话、`DA14`/`DA12` 错误即丢失/不收尾记录、`DA20` selective-save 录错通道、`DA23`+`DA1` TTL 块量化。

## 🔴 Critical

#### `DA1` [Critical · recorder/rhd/types] 数字/辅助通道全链路丢失：TTL、board-ADC、aux 解析后从不落盘/导出/记事件
- [ ] **状态**：✅ 已验证　**定位**：`kv-recorder/src/lib.rs:652-688,943-956 · export_formats.rs:185-221 · kv-types/src/lib.rs:201-204`　**关联 doc19**：无(workflow 2 票 Critical)
- **问题**：parser 完整解析了 `ttl_in/out_per_sample`、`board_adc_data`、`aux_data`(parser.rs:117-120),但 `StreamingRecorder::write_block` 只写 `block.data`(amp),`.rhd` 导出只写 amp 信号组,`AcquisitionEvent::TtlChanged` 全仓库从不构造(events.csv 永远空)。KvrawMetadata 也无这些流的布局字段。
- **在体影响**：这是最致命的在体能力缺口:刺激起始、光遗传脉冲、相机帧钟、行为事件全靠 TTL/ADC 对齐到神经数据。Open Ephys 把所有连续+事件通道写入文件;这里整条事件/同步子系统采下来后被静默丢弃,任何刺激对齐/闭环实验的数据科学上不可用。
- **修复**：扩展 `write_block` 与 KVRAW 格式/元数据持久化 TTL/ADC/aux(记录交织布局);至少写 per-sample TTL,理想镜像 Intan .rhd 块布局;采集 tick 中 diff TTL 字生成 `TtlChanged` 写入 events.csv。加非 None TTL 的 round-trip 测试。

#### `DA2` [Critical · rhd-parser] parser 把任意逐帧异常当致命错误,永久中止整段会话——与 Open Ephys 不同(它重同步/计数继续)
- [ ] **状态**：✅ 已验证　**定位**：`kv-rhd/src/parser.rs:50-68 · backend.rs:172 · kv-core/src/pipeline.rs:237-243`　**关联 doc19**：M16/I7 相关但不同(那是计数器双计/测试覆盖,非致命中止行为)
- **问题**：逐样本循环中:(a)任一样本 32 位 timestamp != 严格递增的期望值,立刻对【整块】返回 `Err(TimestampDiscontinuity)`;(b)任一帧 header != magic 即 `Err(BadMagic)` 且不前向扫描重同步。两者经 read_block→`RhdReadError::Parse`→pipeline `producer_error` 终止采集,会话剩余部分全失,只剩一条泛化错误。
- **在体影响**：在体会话动辄数十分钟~数小时,USB/FIFO 瞬态抖动属常态;把一次可恢复的 timestamp 跳变或字节错位升级为整场实验报废,与黄金参考 Open Ephys(穿过 gap 继续记录)完全相反。叠加 H6 filler bug,单 headstage 在真硬件上会立刻 BadMagic 中止。
- **修复**：不要把块内 timestamp 跳变/BadMagic 当硬解析错:接受观测值、用 integrity/AcquisitionEvent 计数上报;BadMagic 时前向扫描 `RHYTHM_HEADER_MAGIC` 重同步(Open Ephys 做法)。或由 backend/pipeline 把这两类归为非致命(log+continue)。加重同步与块内跳变测试。


## 🟠 High

#### `DA3` [High · rhd-acquisition] 采集热路径无 FPGA FIFO 满度检测——消费慢于实时即在 FPGA 端静默丢帧
- [ ] **状态**：✅ 已验证　**定位**：`kv-rhd/src/backend.rs:794-814,1005-1018`　**关联 doc19**：H2(仅覆盖软件环,未覆盖 FPGA FIFO 溢出)
- **问题**：`read_raw_block` 只用 `wait_for_fifo_words` 等到『够一个块』即读出一个块,从不读 `num_words_in_fifo()` 监控积压是否持续增长。FPGA SDRAM/FIFO 有限深度,消费一旦平均慢于 30kHz 产生速率,FPGA 端填满后丢弃后续帧而 timestamp 继续递增,read_block 零感知。
- **在体影响**：长程记录中磁盘写回/杀软/系统负载周期性拖慢消费;FIFO 一旦溢出,文件里出现无任何标记的整段缺失帧,事后无法区分真实神经静默与丢数,可复现性彻底破坏。Open Ephys 会比对 numWordsInFifo() 与容量并标 dataLost。
- **修复**：成功路径读 `num_words_in_fifo()` 与容量/高水位(如 2~4 块字数)比较,超过即 `log::warn!` + 发 `AcquisitionEvent::BufferOverflow` / DeviceStatus 记硬件溢出计数,上抛给操作者。

#### `DA4` [High · rhd-acquisition] read_block 同步阻塞最长 ~1s,USB 瞬时停顿超时即以 NotEnoughFifoWords 致命中止采集
- [ ] **状态**：✅ 已验证　**定位**：`kv-rhd/src/backend.rs:162-197,1005-1018`　**关联 doc19**：无
- **问题**：`wait_for_fifo_words` 自旋 200×5ms≈1s;其一在生产者线程上长阻塞轮询,其二一旦 USB 瞬停(重枚举/驱动抖动/主机节流)1s 内未达标即 `Err(NotEnoughFifoWords)` 经 `?` 上抛终止整个采集/录制。无重试、无降级、不区分『暂时未就绪』与『真挂了』。
- **在体影响**：在体实验不可重做;USB3 长跑偶发微停顿会让整段实验中途崩断——动物已上头、电极已植入,损失不可逆。
- **修复**：区分超时语义:超时触发有限次重试(并发『采集停顿』事件),连续多次才上抛致命;或把 1s 上限改可配置,每次命中记欠载告警。

#### `DA5` [High · rhd-parser/acq/integrity] u32 FPGA timestamp 回绕(~39.7h)全链路未处理:假 TimestampDiscontinuity + 导出时间轴非单调
- [ ] **状态**：✅ 已验证 / ⏳ 待核实　**定位**：`parser.rs:59-61,111 · backend.rs(零扩展 u64) · kv-integrity/src/lib.rs:136-153,245-260`　**关联 doc19**：H9(packet_id u64 回绕)/M16(gap 双计)相关但均未覆盖 u32 timestamp 域(acq facet 已验证,integrity facet 待核实)
- **问题**：FPGA timestamp 读为 u32,存入 `SampleBlock.timestamp_start` 时仅零扩展为 u64,从不累积跨块 wrap;integrity 用精确 u64 相等比对期望时间戳。30kHz 下 u32 每 ~39.8h 回绕,完全连续无丢的流也会被记为 TimestampDiscontinuity;导出/分析把它当绝对样本索引则得到回绕、非单调的时间轴。
- **在体影响**：多日慢性/睡眠/行为记录是核心在体用例:每 ~40h 至少一次假不连续涌入 integrity 报告,真实钟故障与例行回绕无法区分,侵蚀对完整性档案的信任。
- **修复**：显式处理时间戳域:维护独立的累积 u64 样本钟,或在 2^32 模下比较(`observed.wrapping_sub(expected) as u32 != 0` 才报);加跨 0xFFFFFFFF 边界测试。

#### `DA6` [High · rhd-ffi/acquisition] FrontPanel block-pipe 传输长度未强制为 blockSize(1024B)整数倍——阻抗默认配置即传非对齐长度
- [ ] **状态**：✅ 已验证　**定位**：`kv-rhd/src/frontpanel.rs:208-232 · backend.rs:825,1256 · protocol.rs:165-171`　**关联 doc19**：H5
- **问题**：`ReadFromBlockPipeOut` 契约要求 length 为 blockSize 整数倍;封装把 `buffer.len() as c_long` 直接透传,既不查 `% block_size==0` 也无 debug_assert。阻抗路径默认 freq=1000/sr=30000/periods=20 → total_samples=600,1 stream 时 block_bytes=62400,非 1024 倍数;把 samples_per_block 设为非 128 倍也会让正式采集每块触发。
- **在体影响**：在体记录前必做的电极阻抗自检在默认参数下即向 FPGA 发非对齐块管道读:DLL 行为未定义(整笔拒绝/向下取整/挂起),阻抗值作废或乱码→坏电极被当好电极用于整场记录;或 bring-up 卡死。
- **修复**：封装层加 `if buffer.len() % block_size != 0 { return Err(UnalignedTransfer) }`;调用方分配缓冲按 block_size `div_ceil` 对齐,解析只取 expected 字节。

#### `DA7` [High · rhd-impedance] 阻抗换算省略 Intan 寄生电容/经验校正且无 rail 拒绝——阻抗系统性偏差,railed 通道被当有效
- [ ] **状态**：✅ 已验证　**定位**：`kv-rhd/src/impedance.rs:117,155-178`　**关联 doc19**：无
- **问题**：用理想串联电容模型 `Z=V/(Cs·ω·V_dac)`,Cs 取标称值(0.1/1/10pF)。doc 注释声称『Port of Intan measureComplexAmplitude + approximateSaturationVoltage』,但两项校正都没实现:无经验频率校正曲线(bestAmplitude 表),无 `approximateSaturationVoltage` 的 rail 拒绝。
- **在体影响**：阻抗筛查是决定哪些电极可用的把关步骤;系统性(常随频率)偏差 + 缺 rail 拒绝→好电极被弃、开路/坏电极被纳,绝对值与实验室既有 Intan 台不符,破坏电极 QC 的可信度与可复现性。
- **修复**：移植真正的 Intan 校正:`approximateSaturationVoltage` 检测/标记 railed 通道为 invalid;对幅值施加经验频率校正后再转 Ω;用电阻+电容标准件对照 Intan 台核验。

#### `DA8` [High · rhd-bringup] RHD2164 上半 MISO(ch32-63)半量程门控只查 stream 0,上 32 通道可能 railed/half-scale 仍通过定位
- [ ] **状态**：✅ 已验证　**定位**：`kv-rhd/src/backend.rs:619-654(尤其 644),896-1003`　**关联 doc19**：无
- **问题**：MISO 延迟选定后的半量程(0x4000)居中检查 `amplifier_mean_raw_word(&verify_raw, detected_streams, VERIFY_SAMPLES, 0)` 硬编码 stream=0,只看 ch0-31;且最终对两 stream 套用同一个由 stream-0 数据选出的 delay。(『第二 MISO 从不被延迟扫描』这一更强主张验证者部分存疑,但门控只查 stream0 已确证。)
- **在体影响**：64 通道 RHD2164 的上 32 电极可能整场记录为 half-scale/railed,而 bring-up 报告『headstage located、centering 通过』;科学家事后才发现,丢失不可复现的急性记录。Open Ephys scanPorts 校验每个启用 stream。
- **修复**：居中门控对 `0..detected_streams` 全部 stream 循环,任一 half-scale 即拒;扫描时按芯片类启用整对 stream 并要求 stream1 同样通过 railed/chip-ID 判据。

#### `DA9` [High · rhd-backend/commands/domain] RHD 硬编码 30kHz:configure 无条件调 set_sample_rate_30khz,DeviceConfig.sample_rate 被忽略,落盘 rate 取自 config
- [ ] **状态**：✅ 已验证　**定位**：`kv-rhd/src/backend.rs:255,414-417 · commands.rs(MUX/ADC bias 按 30k)`　**关联 doc19**：L17(吞掉 accepted bool)相关但不同
- **问题**：`configure()` 第255行无条件 `set_sample_rate_30khz()`;虽存在 1k–30k 的完整 M/D PLL 表,configure 从不查 `DeviceConfig.sample_rate`,且 `SampleBlock.sample_rate` 取自 config 而非实际编程值。无任何用户途径选别的硬件采样率。
- **在体影响**：许多在体协议用低率(LFP-only 1–2.5kHz 省盘、或匹配参考台);锁死 30kHz 强制大文件、阻止率匹配对照;若将来改率却不更新元数据来源,落盘 sample_rate 会静默错。
- **修复**：把 `DeviceConfig.sample_rate` 穿入 `configure()`→`set_sample_rate()`,按支持表校验(不支持即报错),并把【实际编程】的率盖到 SampleBlock/KvrawMetadata。

#### `DA10` [High · recorder-format] export_intan_rhd 用合成 0 基 timestamp 计数,忽略 block.timestamp_start——抹平丢包间隔
- [ ] **状态**：✅ 已验证　**定位**：`kv-recorder/src/export_formats.rs:94-104`　**关联 doc19**：无
- **问题**：RHD 时间轴用本地 `ts=0u32` 每帧 `wrapping_add(1)`,完全忽略每个 SampleBlock 携带的真实硬件样本计数 `timestamp_start`。后果:(1)采集期丢包后导出的 .rhd 时间戳仍完全连续,下游看不到 gap 而把 gap 后数据全部时间错位;(2)u32 在 ~39.7h 回绕产生向后跳变,Intan/OE reader 判为损坏/重启。
- **在体影响**：在体 USB/FIFO 抖动丢包属常态;黄金参考 OE/Intan 保留 FPGA 计数使 gap 可见。这里 gap 被静默闭合,围绕 TTL 的 PSTH 等刺激锁定分析恰好错位丢失样本数,且无法察觉。
- **修复**：用首块 `timestamp_start` 播种,每帧发 `timestamp_start + s`(i32,按 Intan 规范),把真实 gap 传到 timestamp 列;文档化并守 >2^31 回绕。

#### `DA11` [High · recorder-format] 导出器(.rhd/flat)不校验块且无界索引 data[]——畸形/部分块 mid-write panic,留下半截『看似有效』的 .rhd
- [ ] **状态**：✅ 已验证　**定位**：`kv-recorder/src/export_formats.rs:96-104,278-287`　**关联 doc19**：C2(尾帧丢失,根因不同)
- **问题**：两个导出器都不调 `validate()/validate_blocks()`(不同于会校验的 write_recording),直接 `block.data[s*channel_count+ch]` 与 `all_samples[sample_idx]`,假定 `data.len()==channel_count*samples_per_channel`。短/长 data Vec(producer/parser bug 或停止时的部分末块)→越界 panic;此时 header 已写、部分数据已 flush。
- **在体影响**：导出常在会话结束对数小时不可复现数据运行;停止时最后一个部分 USB 读的 parser 边界即可让导出线程 panic,留下损坏截断的 .rhd 而操作者以为转换成功。
- **修复**：两个导出器开头 `validate()` 否则 `RecorderError::InvalidBlock`;内层用 `data.get(idx)` 越界即返回错误而非 panic,避免静默留半截文件。

#### `DA12` [High · core-pipeline] 流式错误路径放弃生产者线程且从不 finalize recorder——孤儿采集线程 + 未收尾 .kvraw,且无停止/背压信号
- [ ] **状态**：✅ 已验证　**定位**：`kv-core/src/pipeline.rs:319-324,331-336,352-354`　**关联 doc19**：M5(仅 ProducerFailed 路径)/C1(同区不同缺陷)
- **问题**：消费循环里 `drain_streaming(...)?` 与 `consumer_status(...)?`:integrity.push / recorder.write_block / consumer_status 任一 Err 经 `?` 立即返回。此时 `producer_handle.join()` 永不到达→生产者线程被孤儿化,继续对硬件 read_block 并推入无人排空的 fanout;`recorder.finish()` 永不调用→.kvraw 未写终态元数据。生产者也从无消费侧停止信号。
- **在体影响**：一次可恢复写错误(磁盘满/AV 锁/NVMe 停顿)即把可恢复故障变成不可恢复:孤儿生产者仍对 FPGA 流式而操作者无可见停止;.kvraw 未 finalize 可能不可读/块数错,错误前已采的不可复现数据丢失。
- **修复**：错误退出前先清理:置共享 stop flag 让生产者退出→join→`recorder.finish()`(捕获其错误)使 .kvraw 收尾;用 scope guard;错误里带 partial RecordingSummary。

#### `DA13` [High · integrity-types] integrity.json 的 crc_errors / buffer_overflows 从不写入——完整性档案永远报告 0 损坏
- [ ] **状态**：⏳ 待核实　**定位**：`kv-types/src/lib.rs:239,241 · kv-recorder/src/lib.rs:382-394`　**关联 doc19**：H2(缺 BufferOverflow 事件)相关,但 crc_errors 恒零属独立问题
- **问题**：IntegritySummary 的 `crc_errors`、`buffer_overflows` 原样写入 integrity.json sidecar,但全仓库无任何生产代码给它们赋值。parser 确实检测 BadMagic 帧损坏、buffer 层确实检测溢出,但都从不折回 IntegritySummary——因此恒为 0。
- **在体影响**：在体记录里 integrity sidecar 是研究者/下游判断会话是否可用的依据;真发生 USB 帧损坏或缓冲溢出的会话被存成干净 integrity.json,损坏的神经数据被静默纳入分析——违反『禁止静默错误』。
- **修复**：把真实计数接入 IntegritySummary:backend→integrity 边界把 BadMagic/CRC 计入 crc_errors,fanout/buffer 溢出处递增 buffer_overflows(同 H2 发事件处);暂不能填则序列化为 null 而非误导的 0。

#### `DA14` [High · cli] rhd-smoke / simulator-record 全程在内存缓冲、仅运行结束才写盘——任一 read 错误或崩溃即整段归零,长跑还 OOM
- [ ] **状态**：✅ 已验证　**定位**：`kv-cli/src/lib.rs:333-336,594-598 · kv-core/src/lib.rs:175-200`　**关联 doc19**：H12(Ctrl-C 仅覆盖流式路径)相关但不同
- **问题**：两条命令都经 `run_fixed_blocks`(把每块收进内存 Vec,全部读完才返回),末尾 `write_recording` 一次性写。`run_rhd_smoke` 的 `?` 在任一 `backend.read_block()` 失败时丢弃 0..N-1 所有已采块:无 .kvraw、无 footer、无部分导出。
- **在体影响**：唯一面向硬件的 CLI 命令 rhd-smoke 跑数分钟后崩溃/中断/单次 USB 读停顿 → 输出目录完全空,不可复现的动物数据全损;干净长跑还会 OOM。
- **修复**：走 `run_streaming_pipeline` 增量刷盘,或在传播错误前把已采块持久化;至少给已采块写部分文件并记截断日志、限缓冲块数。

#### `DA15` [High · build-deploy] release profile 未开 overflow-checks——doc19 列举的十余处算术回绕在现场 release 静默产坏数据
- [ ] **状态**：✅ 已验证　**定位**：`Cargo.toml:17-19`　**关联 doc19**：L18/L19/L30/L50 等的构建层系统性兜底(doc19 未涵盖)
- **问题**：`[profile.release]` 只设 lto/codegen-units,全仓库 grep 不到 overflow-checks,故 release 沿用默认 false。叠加 doc19 一批整数缺陷(register_value u8 加法 L19、scan_ports 左移 L18、kv-cli 裸乘 seek L30、read_frames、demo += L50、packet_id 回绕):每处在 release 越界即静默回绕而非 panic,debug 测试反而会 panic——典型『测试过、现场炸』。
- **在体影响**：交付/现场必是 release(gui 别名即 run --release)。一次寄存器配置位回绕→写错 RHD2000 的 ADC/带宽位却无报错,整段记录增益/滤波/映射偏离 Intan 参考而无征兆;seek 回绕→回放/导出错位波形污染 spike/LFP。
- **修复**：`[profile.release]` 加 `overflow-checks = true`(实时热路径代价可忽略),把那批『release 静默回绕』统一转成可观测 panic/可恢复错误;真需回绕处显式 `wrapping_*`,计数器用 `checked_*/saturating_*`。

#### `DA16` [High · domain-completeness] 采集块无 host 墙钟时间戳,无 host↔FPGA 时钟对齐
- [ ] **状态**：✅ 已验证　**定位**：`kv-types/src/lib.rs:46-74 · kv-rhd/src/backend.rs:162-197`　**关联 doc19**：M21(CLI 事件零时间戳)相关但更广
- **问题**：SampleBlock 只带 FPGA 派生的 `timestamp_start`(u32 计数)和 host 生成的 `packet_id`,读块时不捕获任何 host 单调/墙钟时间,也不记录 host↔FPGA 钟映射;KvrawMetadata 连采集起始墙钟都没有。
- **在体影响**：多系统在体台(ephys + 行为相机 + 刺激器各自时钟)靠每块 host 时间戳对齐并量化钟漂;无它则长程记录无法可靠与其他模态合并,FPGA 钟故障改变有效率也无法从文件检出。OE 正为此记软件时间戳。
- **修复**：backend 在 read 后立即给 SampleBlock 加 `host_time_ns`,KvrawMetadata 存采集起始墙钟,并周期记录 (host_time, fpga_timestamp) 对供离线算漂移。

#### `DA17` [High · domain-completeness] 无 channel→物理电极映射 / selective-save 无通道出处写入记录元数据
- [ ] **状态**：✅ 已验证　**定位**：`kv-recorder/src/lib.rs:943-956 · kv-gui/src/app.rs:850-865`　**关联 doc19**：M40(GUI 重复索引校验)相关但不同;并与 GUI 的 DA19/DA20 同根
- **问题**：KvrawMetadata 只记 channel_count,无通道顺序、无 channel→电极/site 映射、无 enabled_channels、无芯片型号/线缆长/bitfile/stream 布局。selective-save 把 `channels: Option<Vec<usize>>` 传给 recorder 过滤列,但 .kvraw 头不存这些列对应哪些原始通道索引。
- **在体影响**：spike sorting 与一切空间分析都需知道哪列对应探针哪个 site;子集 .kvraw 含糊(列 0 可能是 amp ch7 或 ch40),静默破坏下游分析、记录不可复现。
- **修复**：在 KvrawMetadata 持久化保存通道索引向量、完整 enabled_channels、采集出处(芯片型号/enabled_streams/bitfile 名+hash/线缆长/编程率);无通道图的子集记录应拒绝或告警。

#### `DA18` [High · domain-completeness] 录制无磁盘满 / 写慢防护——free space 仅显示从不强制
- [ ] **状态**：✅ 已验证　**定位**：`kv-gui/src/diskspace.rs:37-46 · panels.rs:888 · app.rs:850-885`　**关联 doc19**：无
- **问题**：`free_bytes()` 只接进侧栏显示;`begin_recording` 无预检,流式写循环无低水位、无预计时长检查、无近满自动停。满盘只在事后表现为不透明写错误。
- **在体影响**：在体常是长时无人值守(过夜/行为训练),30kHz×64ch ≈3.7MB/s(~13GB/h);满盘中途即截断进行中的记录。OE 会监控磁盘并优雅告警/停止。丢失不可复现的多小时动物记录是最坏结局。
- **修复**：begin_recording 预检 free space(低于阈值/低于预计会话大小则告警/中止),记录中轮询,低于安全水位时告警 toast + 干净自动停(正常 finalize)。

#### `DA19` [High · gui-signal] Channel Map 面板是死 UI——channel_order 从不应用到显示/FFT/spike/录制
- [ ] **状态**：✅ 已验证(人工)　**定位**：`kv-gui/src/channel_map.rs:182,190 · panels.rs:139(display_to_physical 标 dead_code) · waveform.rs:665`　**关联 doc19**：M40(不同:M40 是 custom map 重复索引)
- **问题**：`channel_order` 仅在 channel_map.rs 设置、仅供预览标签读取;唯一映射函数 `display_to_physical` 标 `#[allow(dead_code)]` 且无调用点;waveform `collect_from_ring` 直接 `phys_ch=start_ch+disp_pos`,FFT/spike/录制路径同样不消费。
- **在体影响**：输入探针 site 映射后界面『确认已应用』,但所有消费者仍用原始采集序——深度剖面、哪条迹对应哪个电极位点全错,且 UI 给正反馈。
- **修复**：在 collect_from_ring 用 display_to_physical 做 disp_pos→physical,并同步应用到 FFT 选择器/spike 列表/(明确)selective-save 索引空间;加『非恒等映射改变物理读取』测试。

#### `DA20` [High · gui-signal] selective-save 会偷偷写入屏幕外通道(visible_channels < channel_count 时)
- [ ] **状态**：✅ 已验证(人工)　**定位**：`kv-gui/src/channel_select.rs:41,62-64,102-112,241,279,424`　**关联 doc19**：无
- **问题**：Rec 勾选框只对 `0..visible`(=`visible_channels.min(ch_count)`)渲染;`selected` 以 `true` 扩容,`selected_indices()` 遍历 `0..channel_count` 缺省 true。默认 visible=16、64ch 时勾『只录子集』并只选 CH0-3,CH16-63 仍全写盘;摘要 `Record n/visible` 用 `.min(visible)` 把多录的藏起来。
- **在体影响**：所见≠所写:要么文件暴涨录了不想要的通道,要么 .kvraw 通道布局与记录的『选择』不符,离线每个 channel→site 映射都偏。
- **修复**：对全部 channel_count 渲染 Rec 行,或把 recording_selection 限定到面板暴露的通道;并显示真实已选总数、对可见窗外存在已选通道告警。

#### `DA21` [High · gui-signal] 满量程/railed 通道被逐窗去直流掩盖,无削顶/railing 指示
- [ ] **状态**：✅ 已验证(人工)　**定位**：`kv-gui/src/waveform.rs:719-728(finalize_channel),642-716`　**关联 doc19**：无
- **问题**：`finalize_channel` 对每通道先减窗口均值再加增益;railed 通道(≈±32767→±1.0 常值)减均值后≈0,渲染成贴基线平直线,与安静的健康通道无法区分;全路径无 `|value|≈1.0` 削顶检测。
- **在体影响**：开机最该做的在线质检——发现饱和/悬空地/坏电极——失效,显示反把故障伪装成干净通道;可能录几小时死数据才在离线发现。OE 把迹钉在轨上提示复查。
- **修复**：去直流前扫描每通道窗口内 `|normalized|>0.98` 的样本比例,超阈值则该 lane 警示色 + 'SAT' 徽标;勿让减均值掩盖。

#### `DA22` [High · gui-app] CAR 把所有通道(含禁用/坏/railed)算进参考,且在 i16 域截断、在高通之前
- [ ] **状态**：✅ 已验证(人工)　**定位**：`kv-gui/src/app.rs:558-570`　**关联 doc19**：M21(被 finder 误标,实为新问题)
- **问题**：CAR 对 `0..ch_count` 求均值并逐样本减,无任何子集/坏道/railed 排除;`(… - mean) as i16`(568)截断(舍入偏置);且 CAR 在 per-channel biquad 之前。
- **在体影响**：一个坏电极污染整阵列参考,把其伪迹(取反、1/N)注入所有通道,干净阵列被显示成整体噪声,引发无谓排障或废弃会话。OE 的 CAR 允许选参考子集正为此。
- **修复**：CAR 均值只在显式参考子集(默认显示启用且非 railed)上算,排除 rail 通道,f64 累加后单次舍入;UI 暴露参考子集;加单坏道 CAR 单测。

#### `DA23` [High · gui-trigger] 触发只按『块』采 TTL(block.ttl_bits),忽略逐样本 ttl_in_per_sample——漏 <~8.5ms 脉冲与块内多边沿
- [ ] **状态**：✅ 已验证(人工)　**定位**：`kv-gui/src/trigger.rs:110-130 · protocol.rs:8`　**关联 doc19**：无(注:gui-lifecycle 的触发停止竞态系 L32 重复,已剔除)
- **问题**：`process_block` 每块只 `current_bit=(block.ttl_bits>>bit)&1` 并每块更新一次 prev_ttl;SampleBlock 的 per-sample TTL 被完全忽略。RHD 块=256 样本,30kHz 下 ≈8.53ms;短于一块的脉冲、块内上升+下降对都看不到,块内两上升沿算一个。
- **在体影响**：光遗传/电刺激/行为 TTL 常亚毫秒~几 ms;触发被量化到 ~8.5ms 块边界并漏脉冲→trial 对齐与刺激日志对不上,科学无效。OE/Intan 按全样本率采 TTL。
- **修复**：遍历 `block.ttl_in_per_sample`(缺失才退回 ttl_bits),跨样本边界保持 prev_ttl,得到样本精确触发时刻。注:需配合 DA1 让 parser 真正保留 per-sample TTL。


## 🟡 Medium

#### `DA24` [Medium · rhd-bringup] 跨端口 best 选择是同类端口里 last-wins——后出现的反射/串扰端口可覆盖真头台
- [ ] **状态**：✅ 已验证　**定位**：`kv-rhd/src/backend.rs(scan_ports best 选择)`　**关联 doc19**：无
- **问题**：扫描各端口选 best headstage 时采用同类(同 chip-class)中 last-wins,没有按信号质量(railed 比例/幅值)择优,后扫到的反射或串扰端口可顶替真实头台。
- **在体影响**：选错端口 → 整场记录对错 SPI 口采集;真硬件多口/长线时可能挑到串扰口。
- **修复**：best 选择按 railed 比例/幅值质量打分择优,而非位置 last-wins。

#### `DA25` [Medium · rhd-bringup] chip-ID 验证过的 delay 即使 railed 比例高也被接受;railed 仅记录从不按 delay 门控
- [ ] **状态**：✅ 已验证　**定位**：`kv-rhd/src/backend.rs(MISO delay 选择)`　**关联 doc19**：无
- **问题**：delay 扫描中只要 chip-ID 读出正确就接受该 delay,mean/railed 只 log 不用于逐 delay 门控;一个 chip-ID 恰好正确但数据大量 railed 的 delay 会被选中。
- **在体影响**：选到边缘 delay → 该会话数据可能间歇 railed/half-scale,信号质量差却通过 bring-up。
- **修复**：逐 delay 把 railed 比例纳入门控(超阈值即拒),在 chip-ID 之外要求幅值在有效窗内。

#### `DA26` [Medium · rhd-impedance] 阻抗档位自选只测 1pF 一次再至多一次,不迭代收敛;阈值与电容档有效范围/寄存器编码不符
- [ ] **状态**：⚠️ 部分争议　**定位**：`kv-rhd/src/backend.rs:1238-1313 · impedance.rs(auto_select_scale)`　**关联 doc19**：M12(v_dac_peak 硬编码,已并入)
- **问题**：`run_impedance_test` 总先在 1pF 测、`auto_select_scale(mag_1pf)` 仅一次、至多再测一次,无切档后的二次收敛检查;且 auto_select 阈值与各电容档有效测量范围、与 `set_zcheck_scale` 寄存器编码不一致。(『1pF 对高阻必 rail』的物理机制验证者部分存疑,故标⚠️。)
- **在体影响**：远离 1pF 甜点的高阻(最具诊断价值)/极低阻(短路)通道被锁在次优电容档,幅值可能差一个数量级或被标错质量。
- **修复**：像 Intan ImpedanceMeasureController 那样测全部三档/迭代收敛,逐通道选幅值落在有效窗的档;切档后重跑 auto_select 直至稳定。

#### `DA27` [Medium · rhd-commands] zcheck DAC 波形对低频静默截断周期(clamp 到 1024)而非报错,畸变阻抗激励
- [ ] **状态**：⏳ 待核实　**定位**：`kv-rhd/src/commands.rs:725-734`　**关联 doc19**：无
- **问题**：`period_samples=(sr/freq).round()` 后 `clamp(1,MAX_COMMAND_LENGTH=1024)`;当 sr/freq>1024 时静默截到 1024,正弦循环只发不足一个周期的命令。Intan 参考检测 `period>MaxCommandLength` 即报『Frequency too low』返回 -1。
- **在体影响**：任何低测试频率(period>1024,如 30kHz 下 <~30Hz)的阻抗扫描,DAC 输出截断的子周期正弦,单频幅相拟合得系统性错误阻抗且无告警,误纳/误弃电极。
- **修复**：对齐 Intan:`period>MAX_COMMAND_LENGTH` 时返回 `Err(FrequencyTooLow)` 让 run_impedance_test 中止/报告,而非 clamp。

#### `DA28` [Medium · recorder-format] .rhd 头把 Nyquist 当上带宽写,并伪造 DSP/带宽滤波参数
- [ ] **状态**：✅ 已验证　**定位**：`kv-recorder/src/export_formats.rs(write_rhd_header)`　**关联 doc19**：H1(0.195 硬编码,字段不同)
- **问题**：.rhd 头里 upper bandwidth 字段写成 Nyquist,且 DSP/带宽滤波相关参数为伪造值,而非实际(或未知应标注)的硬件配置。
- **在体影响**：下游按 .rhd 头解读滤波/带宽时被误导;与真实采集设置不符,影响离线滤波假设。
- **修复**：写真实(或从 DeviceConfig 派生)的带宽/滤波参数;未知则明确标注而非伪造。

#### `DA29` [Medium · integrity-types] SampleBlock::validate() 忽略 side-channel 向量长度与 per-sample TTL 掩码——畸形块通过完整性门
- [ ] **状态**：⏳ 待核实　**定位**：`kv-types/src/lib.rs(validate)`　**关联 doc19**：无
- **问题**：validate 只查主数据长度,不校验 aux/board_adc/ttl_in/out 向量长度是否与 channel/stream/spc 自洽,也不校验 TTL 掩码;字段长度错配的块仍判为有效。
- **在体影响**：畸形块进入记录/导出再 panic(见 DA11)或写出不一致文件;完整性门给出假阴性。
- **修复**：validate 校验所有 side-channel 向量长度与 per-sample TTL 一致性,不一致即 InvalidBlock。

#### `DA30` [Medium · integrity-types] DeviceConfig 类型本身无 validate(),校验只存在于 kv-simulator——非模拟器后端无防护
- [ ] **状态**：⏳ 待核实　**定位**：`kv-types/src/lib.rs(DeviceConfig) · kv-simulator(校验在此)`　**关联 doc19**：L4(SampleBlock NaN 率,doc19 已修)相关
- **问题**：DeviceConfig 没有类型级 validate();唯一校验逻辑在模拟器里,RHD 等真实后端构造 config 时不过同一道校验。
- **在体影响**：非法/不自洽的硬件配置(率、通道、stream)可绕过校验进入 bring-up,后果难定位。
- **修复**：给 DeviceConfig 加类型级 validate(),所有后端构造后统一调用。

#### `DA31` [Medium · cli] benchmark.json 的 duration_seconds 记的是墙钟计算时间而非记录信号时长;requested_duration 从不持久化
- [ ] **状态**：✅ 已验证　**定位**：`kv-cli/src/lib.rs(benchmark 汇总)`　**关联 doc19**：M8(无实时配速)相关但不同
- **问题**：benchmark.json 把 wall-clock compute time 标为 duration_seconds,且从不写入请求的时长,误导基准解读。
- **在体影响**：基准报告的『时长』含义错误,跨配置比较失真。
- **修复**：记录真实信号时长(blocks×spc/sr)并另存 requested_duration。

#### `DA32` [Medium · cli] 所有 record/stream/benchmark 命令 --blocks 默认 1,省略即产出 ~2ms 文件
- [ ] **状态**：✅ 已验证　**定位**：`kv-cli/src/lib.rs(参数默认)`　**关联 doc19**：无
- **问题**：缺省 `--blocks=1`,用户忘传即只采一个块(~2ms),静默产出近空文件而非报错或给合理默认。
- **在体影响**：操作者以为在记录,实际只得 2ms 数据。
- **修复**：默认改为合理值或要求显式 --blocks/--duration,缺失即报错。

#### `DA33` [Medium · build-deploy] okFrontPanel.dll 以全路径 LoadLibrary 加载,其传递依赖 DLL / VC++ 运行库不在搜索路径
- [ ] **状态**：⏳ 待核实　**定位**：`kv-rhd/src/frontpanel.rs(LoadLibrary) · third_party/opalkelly`　**关联 doc19**：M38(DLL 无 NOTICE)相关
- **问题**：用全路径加载 okFrontPanel.dll,但它自身依赖的其他 DLL / VC++ 运行库未保证在搜索路径,目标机缺运行库时加载失败。
- **在体影响**：现场新机器上 bring-up 直接因 DLL 依赖缺失失败,排查困难。
- **修复**：随附必要运行库或文档化前置依赖;用 SetDllDirectory/AddDllDirectory 确保依赖可解析。

#### `DA34` [Medium · build-deploy] 未声明 panic 策略且采集/GUI 主线程无 catch_unwind 隔离——一次主线程 panic 即终止进行中的记录
- [ ] **状态**：⚠️ 部分争议　**定位**：`Cargo.toml:17 · kv-gui update() 驱动 live pipeline`　**关联 doc19**：H18/L33/doc19 主题6(铁律4)
- **问题**：release 未设 panic 策略(默认 unwind);kv-gui 在 eframe 主线程 update() 直接驱动 live pipeline,主线程一旦 panic(notch_idx 越界/fft 非二幂/unwrap)即结束事件循环→整进程退出,连带正在写盘的 recorder。无 catch_unwind/set_hook/隔离。(单点 panic 系 H18/L33 重复,此为构建层 panic 策略视角。)
- **在体影响**：实验进行中某 GUI 面板触发 panic 即让 .kvraw 失 footer/正常 flush,可能损坏或截断,动物在线数据报废。
- **修复**：明确 panic 策略:abort 配合采集/落盘线程内 catch_unwind 保证收尾;或 recorder 独立线程 + panic hook 优先收尾。

#### `DA35` [Medium · domain-completeness] packet_id 由 host 生成而非硬件派生——FPGA FIFO 丢失/重启对 integrity 不可见
- [ ] **状态**：⏳ 待核实　**定位**：`kv-rhd/src/backend.rs(packet_id 递增) · kv-integrity`　**关联 doc19**：H9/L22 相关但不同
- **问题**：packet_id 是 host 端递增计数,不来自硬件;FPGA 端 FIFO 丢帧或计数重启时 host packet_id 仍连续,integrity 的 packet 连续性检查看不到硬件级丢失(应靠硬件 timestamp 跨块连续性,而那也未做,见 DA5)。
- **在体影响**：硬件级丢数被 host 伪连续 id 掩盖,integrity 报告假阴性,与 DA3(FIFO 溢出无检测)叠加使丢数完全不可见。
- **修复**：用硬件 timestamp 跨块连续性核验丢失,或把 FPGA 计数纳入 packet 连续性,而非纯 host id。

#### `DA36` [Medium · gui-signal] spike_overlay::process_block 无边界检查直接索引 block.data——短/部分块即 panic
- [ ] **状态**：✅ 已验证(人工)　**定位**：`kv-gui/src/spike_overlay.rs:269-274`　**关联 doc19**：无
- **问题**：`block.data[s*ch+c]`(269-274)无 `data.len()>=spc*ch` 检查,而 disp_ring.rs:144 对同样访问有 `if idx<data.len()` 守卫——代码库不一致。截断/部分块(USB 停顿、起停瞬间)→ GUI 线程越界 panic。
- **在体影响**：spike-overlay/AP band 活动时一个畸形块即 panic GUI 线程,按铁律4 不得拖垮采集/收尾;单块即可中止不可复现记录。
- **修复**：`block.data.get(idx).copied().unwrap_or(0)` 或 `data.len()<spc*ch` 早退;同样修 channel_select 与 CAR 循环。

#### `DA37` [Medium · gui-render] 波形 spike 徽标在二次抽取的显示环上检测——宽窗口下混叠,计数随缩放变
- [ ] **状态**：✅ 已验证(人工)　**定位**：`kv-gui/src/waveform.rs:678-703 · disp_ring.rs:206-271`　**关联 doc19**：H15(仅修不应期单位;此为抽取混叠,不同。另注:本分支 waveform.rs:684 不应期仍用满采样率,即 H15 在此分支未修)
- **问题**：检测跑在 `ring.collect_channel` 返回的点上,该数据经 RING_DWNSP=4 + 渲染期 stride2=window/max_points 两次抽取;宽窗(如 60s)每点跨数百样本,1ms spike 落在采样点之间;sigma 也由抽取后(可能 LFP 主导)数据导出。
- **在体影响**：在体用徽标做活动确认/探针定位时,计数随时间窗缩放变而非随放电率变→错误的探针放置判断或假阴性『此区静默』。
- **修复**：在专用 spike-band、最小抽取的 AP 环/snippet 流上检测,或仅在窄窗(stride2 小)启用徽标;检测采样率显式独立于渲染缩放。

#### `DA38` [Medium · gui-trigger] 触发状态跨采集停止/启动或切源从不复位——陈旧 prev_ttl 与 Triggered 态泄漏到下一会话
- [ ] **状态**：✅ 已验证(人工)　**定位**：`kv-gui/src/app.rs:337-359,366-393,412-440,775-789`　**关联 doc19**：无
- **问题**：`self.trigger` 只在 ingest_block 与侧栏 UI 触及;start_demo/start_device/stop_all/select_source 均不 `disarm()`/重置 prev_ttl。停在 Triggered 态后重启,新会话首块基于陈旧 TTL 历史→假边沿触发停止,或因 prev_ttl 已等于新 bit 而漏首个真实边沿。
- **在体影响**：研究者常在 trial/动物间停启采集;泄漏的 Triggered 态使新会话首个刺激不被捕获或立即自动停,丢实验开头且无报错。
- **修复**：在 stop_all/start_*/select_source 调 `trigger.disarm()` 并重置 prev_ttl;会话首块作为边沿检测预热(不动作)。


## 🔵 Low

- [ ] `DA39` **[rhd-parser]** ttl_bits 仅校验末样本,per-sample TTL 字从不校验,ttl_bits 语义有损　〔✅ 已验证〕　`kv-rhd/src/parser.rs(ttl 处理)`
  　**问题**：block.ttl_bits 只取末样本的 TTL,per-sample TTL 不参与校验且块级 ttl_bits 丢失块内变化。　**在体**：与 DA1/DA23 同根:数字事件被块量化。　**修复**：保留并校验 per-sample TTL,ttl_bits 仅作便捷摘要。　**关联**：DA1/DA23

- [ ] `DA40` **[rhd-bringup]** set_cable_length_meters 用 DEFAULT_RHD_SAMPLE_RATE 而非配置率计算 delay　〔✅ 已验证〕　`kv-rhd/src/backend.rs(set_cable_length_meters)`
  　**问题**：线缆延迟换算用默认采样率常量,非实际配置率;非 30kHz 时 delay 偏。　**在体**：与 DA9(30kHz 硬编码)叠加;改率后线缆 delay 错算。　**修复**：用实际编程率计算 cable delay。　**关联**：DA9

- [ ] `DA41` **[rhd-acquisition]** 惰性首启不复位 FPGA timestamp 计数器,首块时间戳承袭 bring-up 残值　〔✅ 已验证〕　`kv-rhd/src/backend.rs(start_continuous_acquisition 惰性首启)`
  　**问题**：read_block 内惰性首启时不复位 FPGA timestamp,首块 timestamp_start 带 bring-up 期残值。　**在体**：首块时间基准偏移,影响绝对时间轴起点。　**修复**：首启时复位 FPGA timestamp 计数器或记录起始偏移。　**关联**：DA5

- [ ] `DA42` **[rhd-commands]** register_value() 对 reg6 返回硬编码 128,并把越界错误误标为 RegisterRead　〔⏳ 待核实〕　`kv-rhd/src/commands.rs(register_value)`
  　**问题**：reg6 返回硬编码 128,越界寄存器号被误标为 RegisterRead 错误类型。　**在体**：调试/校验寄存器时误导。　**修复**：reg6 用真实值,越界返回恰当错误类型。　**关联**：L19

- [ ] `DA43` **[integrity-types]** 首个观测块之前丢失的包从不计入 missing　〔⏳ 待核实〕　`kv-integrity/src/lib.rs`
  　**问题**：完整性从首个观测块开始计数,会话最开头(首块之前)丢失的包永不计入。　**在体**：会话起始丢包被漏报。　**修复**：以期望起始 packet_id/timestamp 为基准核算首块前丢失。

- [ ] `DA44` **[gui-playback]** 回放 tick 高倍速跳帧:总读以 cursor 结尾的固定块,frames_to_advance>block_frames 时块间数据从不读　〔✅ 已验证(人工)〕　`kv-gui/src/playback.rs:161-194`
  　**问题**：cursor 按 dt*sr*speed 前进,却总读以 cursor 结尾的固定块;高倍速/卡顿后两窗之间样本永不读取也不进 spike/trigger。　**在体**：以>1x 倍速刷查录制会跳过文件里真实存在的事件(回放完整性)。　**修复**：读 [prev_cursor,cursor) 全段(必要时分块),勿固定块。　**关联**：M42/L51


## 附录 A · 与 doc 19 重复（已剔除）

- [rhd-parser] parser 读 helper 越界 panic —— 重复于 **doc19 §5 + P1 list(parser.rs:193-222)**
- [rhd-ffi] read_from_block_pipe_out 正向短读被当成功 —— 重复于 **H5(两条真实采集读路径已检测短读)**
- [rhd-ffi] get_device_list_serial 未传缓冲长度 —— 重复于 **H3**
- [rhd-impedance] v_dac_peak 硬编码 128 忽略 config.dac_amplitude —— 重复于 **M12(已并入 DA26)**
- [recorder] parse_kvraw_json 首子串匹配 / v2 接受非法 JSON —— 重复于 **L27**
- [pipeline] 生产者 panic 毒化 Mutex 级联跳过 finish() —— 重复于 **M5(且 read_block 不在持锁时调用,机制部分不成立)**
- [gui-lifecycle] 触发 StopRecording 与异步 Stopped 竞态 —— 重复于 **L32**
- [gui-lifecycle] stop_all 不 finalize Armed / begin_recording 无 pipeline 静默成功 —— 重复于 **M26**
- [gui-lifecycle] remote StopAcquisition 绕过 recorder finalize —— 重复于 **L32**

## 附录 B · 对抗验证驳回（流程未放水的佐证）

- [rhd-bringup] 半量程门控带 0x3000..0x5000 可假通过/假失败
- [rhd-ffi] ConfigureFPGA 前未检查 IsOpen / 空 bitfile 仅 warn
- [rhd-impedance] compute_impedance 对 railed 返回有限阻抗(已并入 DA7 修复)
- [recorder] finish() 在 JSON 头 >512B 时中途截断
- [recorder] KvrawReader 丢尾部部分帧 / 错误去交织
- [pipeline] drain 循环吞掉 pop() 的 Err
- [cli(相关 L15)] 模拟器不填 aux/ADC/TTL 使 CLI 路径未测
- [cli] rhd-smoke raw 路径强制 packet id 从 0 / 忽略尾字节

## 附录 C · 跨根因合并说明

- `DA1` 合并：domain(.kvraw 不写 TTL/ADC/aux) + recorder(.rhd 导出丢弃) + domain(TtlChanged 从不发) + parser(ttl_bits 有损)。
- `DA2` 合并：parser 块内 timestamp 跳变致命 + BadMagic 不重同步致命（同一『逐帧异常即中止会话』根因）。
- `DA5` 合并：parser(u32 截断) + acquisition(零扩展回绕) + integrity(假 TimestampDiscontinuity)。
- `DA6` 合并：ffi(封装不校验对齐) + acquisition(块字节非 1024 倍)。
- `DA9` 合并：backend(30kHz 硬编码) + commands(寄存器按 30k) + domain(率不可配)。
- `DA12` 合并：pipeline(错误路径孤儿生产者+不 finalize) + pipeline(无生产者停止/背压信号)。
- `DA14` 合并：cli(全内存缓冲末尾写) + cli(单次 read 失败丢弃全部已采块)。

---

_本清单由多智能体深度审计 + 人工复核生成。逐项修复请勾选对应框并在 PR 引用编号（如 `fix DA1, DA12`）。⏳ 项请先人工复核再动手。_
