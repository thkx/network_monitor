# ADR-0007：架构级设计缺陷盘点与分批治理方向

- 状态：已接受（2026-10-11 回填第二轮多 Agent 评审结论后确认）
- 日期：2026-10-11
- 决策者：项目维护者
- 相关：[ADR-0001](<0001-adopt-adr-and-multi-agent-workflow.md>)（多 Agent 工作流）、[ADR-0002](<0002-unify-async-threading-model.md>)~[ADR-0006](<0006-security-hardening.md>)（已记录的性能/安全改造）

## 背景（Context）

ADR-0002~0006 聚焦**性能热路径与安全加固**，并已实现验证。应「重新分析设计缺陷」的要求，本次用「多 Agent 开发模式」对代码库做了一次**架构级**复盘：

- **第一轮（缺陷盘点）**：按 4 个分区各派一个只读审查 Agent（监控引擎/调度、告警子系统、数据库/领域、API/鉴权/配置/可观测性），只报告 ADR-0002~0006 **之外**的架构/设计层面缺陷，每条给出 位置、缺陷、影响、严重度、建议方向。产出下方 A~E 簇。
- **第二轮（评审讨论）**：又派 4 个讨论 Agent 从 优先级/ROI、现状辩护与误报、实施风险与依赖排序、遗漏补充 四个角度评审第一轮结论。产出：严重度**校准**（见「评审校准」标注）、**有意取舍**清单、**评审补充缺陷**（F/G 组）、**治理方案自身的新风险**（R 组），以及**收敛后的分批执行序**。

本 ADR 的定位：**权威缺陷清单（inventory）+ 分批治理排期**，不在本条内直接实现。具体修复按批另起 ADR（0008+），每批在「相关」栏显式标注与被深化/修订/取代的旧 ADR 的关系。

说明标记：
- 严重度：高 / 中 / 低。括注「评审校准：…」表示第二轮据单实例自托管定位调整了评级或触发前提。
- `★`：由 ADR-0002~0006 的改造**引入或遗留**、与其意图相悖的新债务。
- `⊕`：被多个 Agent **交叉确认**，置信度更高。
- 项目定位锚点（评审共识）：**单实例、自托管、单管理员**；写入由**单消费者任务串行**；对外 API 唯一消费者是打包进同一二进制的单页控制台。许多缺陷的严重度在"多实例/公共 API/高并发写"假设下才成立。

## 缺陷清单（Decision 的事实依据）

### A 簇 · 运行时监督、任务生命周期与可靠性

| 编号 | 缺陷 | 位置 | 影响 | 严重度 |
| --- | --- | --- | --- | --- |
| A1 | **单消费者 + 单通道是全局单点**：所有监控任务 `send().await` 到唯一 bounded(100) 通道、唯一 `consume_results` 消费 | `main.rs:188`、`main.rs:311-365`、`async_monitor.rs:74` | 消费者 panic→所有 send 返回 Err→任务逐个退出、监控全盘静默消失；消费者变慢→通道灌满→探测阻塞 | 高（评审校准：灾难面几乎全由 A6 触发，低流量下"慢消费者灌满"概率低；**修掉 A6 后 A1 的全盘停摆面大半关闭**，剩余的消费者监督随 A2 处理） |
| A2 | **任务无监督、无重启**：spawn 的 interval 任务 panic 后，JoinHandle 仍留在 `Scheduler.tasks`，无人观测 JoinError | `async_monitor.rs:46`；各 `check` 内 `.lock().unwrap()`（`cpu.rs:36/45/49`、`disk.rs:32`、`memory.rs:26`、`process.rs:26`、`http.rs:49`） | 单个监控永久静默停产，而 `active_task_count` 仍计数、列表仍显示"存在" | 高（评审确认，紧随 A6） |
| A3 | **热更新非原子**：`reload_all` 先 `abort` 全部任务，**之后**才读库；DB 读失败即 `return` | `scheduler.rs:45-64` | 旧任务已毁、新任务未建→一次瞬时 DB 抖动使全量监控清零且不自愈 | 高（评审确认，build-then-swap 修复廉价且显然正确） |
| A6 ⊕★ | **后台任务仍以 `.expect("锁中毒")` panic**：消费循环与会话清扫未纳入 ADR-0006 E2 的中毒恢复覆盖面 | `main.rs:339`（消费者 `metrics.lock().expect`）、`main.rs:221`（sweep） | handler 侧对同一把锁用恢复、后台侧用 `.expect`：锁一旦中毒，消费者下次 `record()` panic→全盘停摆且进程探活仍绿，故障隐蔽 | 高（评审确认为**头号必改**，修复极廉价） |
| A4 | **关停对探测侧非优雅**：优雅关停只体现为"等消费者攒批落库"，监控任务被 `abort()` 硬砍 | `scheduler.rs:76-81`、`main.rs:269-276` | 在途探测（含 ping/tracert 子进程、在途 send）被硬取消、结果丢失 | 中→低（评审校准：DB 权威、重启重跑，影响小） |
| A5 ★ | **全量重建重置无关监控热态**：改任一条配置都 abort+重建**所有**任务 | `scheduler.rs:45-62` | ADR-0003 引入的每实例复用状态（CPU 预热、DNS resolver、HTTP 正则缓存）被清零 | 中→低（评审校准：reload 仅管理员手动改配置时低频发生；增量调度成本高回报低，**归入有意取舍/延后**） |

### B 簇 · 领域模型、存储边界与一致性

| 编号 | 缺陷 | 位置 | 影响 | 严重度 |
| --- | --- | --- | --- | --- |
| B1 ⊕ | **全项目缺领域错误类型**：`diesel::result::Error` 贯穿全栈外泄；`pool` 把"连接池耗尽/超时"硬塞进 `QueryBuilderError`；工具层用裸 `Box<dyn Error>`（非 Send/Sync）+ 字符串错误 | `pool.rs:96-101`、`response.rs:41-62`、`tools/http.rs:54-57` | 唯一冲突/NotFound/池耗尽/查询 Bug 全坍缩成不透明枚举，上层无法分类/重试/告警；需识别 409 的 handler 被迫绕开 helper 重写样板 | 高（且为 D2/B4/B12 的**前置枢纽**） |
| B2 | **监控配置双重真相**：整份 `MonitorDefinition` 序列化进 `config_json`，同时关键字段又作独立列落库 | `services/mod.rs:25-36`、`models.rs:41-51`、`scheduler.rs:86` | — | 高→**中（评审校准，已核实降级）**：PUT 全量更新（`monitor_handlers.rs:244`）**同步重写 config_json 与各列**，`set_enabled`（`monitor_service.rs:36-51`）只改 `enabled` 列而 `enabled` 不在 blob 中——**当前不会分叉**。真实风险是"两份表示无 DB 级约束保证一致"的**潜在脆弱**（未来若新增只写列的 writer 才分叉），非现存 bug |
| B3 | **缺防腐层**：diesel `Queryable` 结构体直接派生 `Serialize/Deserialize`，DB 列布局=对外 JSON 契约；无存储→领域反向映射 | `models.rs:7-8/53` | DB schema 演进直接破坏 API 契约；领域富类型信息往返丢失 | 中高→中（评审校准：自带控制台单消费，全层防腐低回报；若需隔离，轻量做法是仅对外字段加 DTO） |
| B4 | **全栈无事务边界**：无一处 `conn.transaction` | `monitor_repo.rs:43-48`（count+list）、alert_state+alert_history 跨表双写 | count+list 并发可"总数 11 翻出 10 条"；跨表写中途崩溃不一致 | 中高→中（评审校准：单消费者串行写下 count+list 近乎 cosmetic；跨表 state+history 不一致才是真点、但频率低） |
| B5 ⊕ | **同一配置两条并行转换 + interval 单位漂移**：`build_monitor_insert` 与 `MonitorConfig::from_entry` 各自推导默认值；一处 `*1000` 存毫秒、一处按秒；`timeout.max(1000)` 在 5+ 监控里逐个手写 | `services/mod.rs:10-37`、`check.rs:26-156/151`、`tcp.rs:36`/`icmp.rs:71`/`dns.rs:50`/`udp.rs:54`/`ftp.rs:43` | 默认值/单位漂移风险；且是 B2 分叉的**机制根因** | 中 |
| B6 ⊕ | **Service 层空壳 + 分层不一致**：monitor/result 走 service，alert/status/metrics 在 handler 内直接从 pool new 仓储 | `monitor_service.rs:17-18`、`alert_state_repo.rs:8`、`alert_handlers.rs:30-33`、`status_handlers.rs:49`、`metrics_handlers.rs:19` | 持久化边界被击穿；事务/不变量/错误翻译无处安放 | 中 |
| B7 | **`get_latest_by_monitor` 用无界 IN 列表**（参数数=监控数） | `result_repo.rs:72-84` | 撞 `SQLITE_MAX_VARIABLE_NUMBER` 报错 | 中（评审校准：SQLite 3.32+ 上限 32766、旧版 999，需约 1000+ 监控才触发；自托管少见，属 latent） |
| B8 ⊕★ | **保留策略单条无界 DELETE** 作用在最热大表 | `result_repo.rs:28-34` | 长事务独占写锁**阻塞单消费者写入链**并撑大 WAL，与 ADR-0005 相抵 | 中→**实践高（评审上调）**：长跑实例每 24h 必然触发，修法（分批删）极廉价，性价比最高之一 |
| B9 | **领域不变量裸 i32/String 无 newtype**；读模型 `CheckResultModel` 误标 `Insertable` 且含 id/时间戳 | `models.rs:53-64` | enabled/alerting/status 为 i32、monitor_type 读回 String；类型上允许绕过 AUTOINCREMENT/CURRENT_TIMESTAMP | 中 |
| B10 | **时间表示脆弱**：裸 `Option<NaiveDateTime>` 无时区、UTC 约定隐式；可空列恒有 DEFAULT→幽灵 Option；只增不改的 `check_result` 仍带 `updated_at` 列+触发器 | `result_repo.rs:30-31`、`up.sql:14-15/49-54` | 时区混淆；Option 污染全调用点；热表冗余触发器 | 中 |
| B11 | **分页契约碎片化**：OFFSET+COUNT 与 keyset 并存，游标为裸 `Option<i32>`，无 `Cursor`/`Page<T>` 封装 | `monitor_repo.rs`、`result_repo.rs`、`response.rs:14-37` | 分页语义不统一，调用方按端点分别处理 | 低 |
| B12 | **仓库硬绑 `Arc<SqlitePool>` 无 trait 抽象** | `database/repositories/*` | 无法脱离真实 SQLite 测试、后端被钉死 | 低→**有意取舍**（评审：无换库诉求，且测试已跑真实 SQLite，加 trait 属投机性泛化） |
| B13 | **`monitor_config.name` UNIQUE 建在可空列** | `up.sql:5` | SQLite 多 NULL 互异→可插多条无名记录使去重静默失效 | 低（修法小、建议随 Batch 0 顺手做） |

### C 簇 · 告警子系统语义

| 编号 | 缺陷 | 位置 | 影响 | 严重度 |
| --- | --- | --- | --- | --- |
| C1 | **EMAIL 乐观 Delivered 静默丢告警**：抢到信号量许可即返回 `Delivered`，后台重试全败仅打 error | `notify.rs:127-157`、`engine.rs:156-167` | `alerting=true` 据此置位并抑制后续重发；SMTP 持续故障时用户收不到告警且系统自认已告警 | 高（评审确认为**必改**，击中监控工具唯一职责） |
| C2 | **单一 `alerting` 位覆盖一组异构规则 + evaluate 首命中即返回** | `engine.rs:30`、`engine.rs:209-255` | 规则互相掩盖；多个独立故障折叠成一次 triggered/recovered | 中（多规则下趋高；单规则监控无影响） |
| C3 | **通知渠道硬编码巨型 match，`AlertSender` 抽象名存实亡**：webhook/EMAIL 两路重复整套后台重试样板 | `notify.rs:85-216`、`domain/alert.rs:142-163` | 新增渠道要改多处，违背开闭；渠道不可单测 | 中 |
| C4 | **告警事件过早坍塌为展示字符串**：engine↔notify 以 `String` 为契约，恢复靠 `message.starts_with(前缀)` 嗅探 | `engine.rs:157/181-188`、`notify.rs:106` | 渠道拿不到结构化字段，无法渲染富格式；语义靠前缀传递脆弱 | 中 |
| C5 | **半持久化状态机**：`alerting` 持久化、去抖计数 hit_streak/ok_streak 仅内存 | `engine.rs:31-37/90-98`、`scheduler.rs`（reload `with_state` 重建） | 重启/热更新时去抖窗口断裂，告警被额外延迟 `N×interval` | 中→低（评审：优先显式声明"去抖是进程内瞬时语义"为取舍；若持久化见 R4 写放大风险） |
| C6 | **alert_history 仅在同步 Delivered 时写入**：后台送达/Deferred/最终失败均不入史 | `engine.rs:162-167/194-199` | 历史表无法反映真实送达，通知失败对控制台不可见 | 中 |
| C7 | **两套重试机制重叠**：notify 后台退避 + engine"下轮重发" | `engine.rs:171`、`notify.rs:267-295` | 抖动时两条恢复路径可能都成功→重复告警；无单一事实来源 | 中 |
| C8 | **多实例无协调**：alert_state 无 owner/租约/版本/锁 | `alert_state_repo.rs:40-57`、`async_monitor.rs:36-37` | 多实例→重复告警风暴 + upsert 相互覆盖 | 中→**有意取舍**（评审：单实例定位下为非目标；显式声明单实例约束，不实现分布式协调） |
| C9 | **NotifyCondition 积类型大杂烩 + alert_history.alert_type 实为 monitor_type** | `domain/alert.rs:18-31`、`engine.rs:163/195` | 非法组合靠运行时守卫；历史 `alert_type` 名实不符 | 中 |
| C10 | **SMTP transport 缓存无界、键含明文口令、无 TTL/驱逐** | `tools/smtp.rs:24-33/75-90` | 缓存只增不减→内存泄漏；明文口令长期驻留 | 低（随 Batch 0 顺手做） |
| C11 ★ | **webhook_client 构建失败 `expect` panic**（ADR-0003 已修 HttpMonitor 同类，此处遗留） | `notify.rs:33-42` | 首个 webhook 告警触发时若 Client 构建失败则 panic | 低（一行级，对齐 ADR-0003） |

### D 簇 · API 层、启动装配与可观测性

| 编号 | 缺陷 | 位置 | 影响 | 严重度 |
| --- | --- | --- | --- | --- |
| D1 ⊕ | **MetricsRegistry 双重加锁**：内部已有 `Mutex`，装配层又包进 `Arc<Mutex<MetricsRegistry>>` | `metrics.rs:58-60`、`main.rs:183`、`metrics_handlers.rs:24-27` | 两把锁恒冗余、徒增争用，两层中毒恢复策略不一致（接 A6） | 高（评审确认，与 A6 **改同几行、应合并一次做**） |
| D2 | **统一响应信封名不副实**：成功走 `{code,message,data}`，4xx/5xx 回退纯文本，401/403/409 又各自手写 JSON | `response.rs:41-44`、`validation.rs`、`monitor_handlers.rs:134/190-198`、`auth.rs:426-439` | 同一 API ≥3 种响应形状；前端无法统一解析 | 高→中（评审校准：DX/一致性债，不致 outage/丢数据；且**依赖 B1 先行**） |
| D3 | **配置来源分散、无集中 Config**：`BIND_ADDR` 被 auth（推断 cookie Secure）与 main（实际 bind）两处各读 | `auth.rs:84-118/107`、`main.rs:228`、`logging.rs:22`、`args.rs:20-27` | 若 bind 解析方式演进，Secure 推断会与真实 bind 背离 | 高→中（评审校准：两处当前读同一 env、行为一致，**现状无缺陷**，属 latent 可维护性） |
| D4 | **类型键式 DI 脆弱、无 AppState**：`web::Data<u64>` 用裸基元作键；包裹层数错误能编译却运行期 500 | `main.rs:232-244` | 装配错误运行时才暴露；handler 测试需手工重注 | 中 |
| D5 | **校验未集中**：`validate_config` 仅 API 路径调用，`import_monitor_list` 启动导入不校验 | `validation.rs:11`、`monitor_handlers.rs:165/221`、`main.rs:427-446` | 脏配置经 JSON 文件直接入库，运行期才暴露 | 中 |
| D6 | **CSV 与 DB 治理不对称**：DB 有保留清理，CSV 按日生成文件后永不回收 | `csv_logger.rs:45-56`、`main.rs:158-180` | 长跑 CSV 目录无界增长撑满磁盘，连带打死权威 DB | 中→**长周期高（评审上调）**：易触发、爆炸半径大、修复极简 |
| D7 ⊕ | **自实现 Prometheus 注册表 + 单一全局写锁** | `metrics.rs` 全文、`metrics.rs:58-60` | exposition 规范需人工维护；单锁在高基数下成写入热点 | 中→低（评审校准：自托管量级单锁不成瓶颈；且 D7 与 D1 是**替代关系**——先做 D1，D7 后续取代） |
| D8 ⊕ | **run_monitor_once 在 handler 内重复消费循环编排** | `monitor_handlers.rs:364-455`、`main.rs:333-354`、`async_monitor.rs:57-63/93-99` | 两份"指标+落库"易漂移，难单测 | 中 |
| D9 | **缺少 API 版本化** | `api/mod.rs:22-23` | 信封演进破坏调用方 | 低→**有意取舍**（评审：唯一消费者是同一二进制的控制台，无外部契约，加版本前缀是镀金） |
| D10 | **无就绪探针**：liveness 不查依赖（合理），无免鉴权 readiness | `health_handlers.rs:1-10`、`auth.rs:342-353` | DB 池耗尽时 `/healthz` 仍绿，编排器不摘流 | 低（新增 `/readyz`，随 Batch 0） |
| D11 | **内存会话 + 内存指标无法水平扩展** | `auth.rs:1-7/181-187`、`metrics.rs` | 限定单实例部署 | 低→**有意取舍**（评审：显式声明单实例约束并记入部署文档，不实现外部会话/指标聚合） |

### E 簇 · 监控抽象与类型（以 HTTP 为中心的渗漏）

| 编号 | 缺陷 | 位置 | 影响 | 严重度 |
| --- | --- | --- | --- | --- |
| E1 | **`Monitor` trait 抽象弱**：`(bool, CheckResultDetail)` 二值返回语义跨实现漂移，bool 兼表"执行失败"与"目标不可用"，无类型化错误 | `monitor/mod.rs:14`、`http.rs:439/462/503/512`、`tcp.rs:21/43`、`unknown.rs:26` | 调用方拿 bool 直接当 `status` 落库，语义不统一 | 中（DISJOINT 友好：冻结 trait 后 12 monitor 可扇出） |
| E2 | **结果模型 HTTP 中心化**：资源类监控走兜底 `(status, 0, None)`，真实度量不投影到通用字段，`status: bool` 无 degraded | `domain/check.rs:242-265` | CPU/MEM/DISK 的阈值告警、看指标/CSV 基本是坏的 | 中→**功能性缺口（评审）**：对一个宣称支持资源监控的工具是真实功能缺陷（修复成本大，归专批） |
| E3 | **命令类监控硬编码外部二进制**：直接 spawn `ping`/`tracert`/`traceroute` 并解析跨 locale stdout | `icmp.rs:91-106/25-58`、`traceroute.rs:36-74` | 无命令执行抽象→不可注入测试，依赖目标机二进制与系统语言 | 中 |
| E4 | **`MonitorConfigDetail` 11 个空变体 + 工厂与详情双真相源** | `domain/check.rs:161-206`、`monitor/mod.rs:53-68`、`http.rs:446`、`unknown.rs:20` | 同一"类型→行为"用两套结构表达可能漂移 | 低→**有意取舍/低优**（评审：当前显式且类型安全，合并仅省样板） |
| E5 | **ID 模型不一致**：`CheckResult.id` 为 u128，内容校验 rule_id 截断为 u64 | `async_monitor.rs:58/95`、`http.rs:259`、`main.rs:457` | 跨 CSV/DB 关联 ID 类型不一 | 低 |

### F 簇 · 构建 / 依赖卫生（第二轮评审新增，`G4`）

| 编号 | 缺陷 | 位置 | 影响 | 严重度 |
| --- | --- | --- | --- | --- |
| F1 | **为借 `csv::Writer` 整包依赖 `prettytable`** | `csv_logger.rs:3`（`use prettytable::csv::Writer`）、`Cargo.toml:9` | 应直接依赖 `csv` crate，去掉重依赖 | 低 |
| F2 | **reqwest `blocking` 特性疑似未用**：全库 `reqwest::blocking` 0 命中 | `Cargo.toml:12` | 异步项目拉入未用的阻塞客户端+线程池，增大构建面 | 低（需核实后再去特性） |
| F3 | **无 `rust-version`/MSRV 锁定**，却用 `edition = "2024"` | `Cargo.toml` | 构建可复现性/工具链前提未记录 | 低 |
| F4 | **tokio `full` + `libsqlite3-sys` bundled** 等构建面取舍未记录 | `Cargo.toml` | 编译需 C 工具链；特性可收敛 | 低 |

### 评审补充缺陷（第二轮多 Agent 讨论新增，均带 file:line 佐证）

| 编号 | 缺陷 | 位置 | 影响 | 严重度 |
| --- | --- | --- | --- | --- |
| G1 | **Once 模式下 `consecutive_failures>1` 的去抖告警永不触发**：单次运行 hit_streak 最多到 1 | `main.rs:81-96`、`engine.rs:149-156` | 同一份 alert_rules 在 Once vs Monitor/Server 行为不同——跨模式告警语义不一致 | 高 |
| G2 | **缺集成测试层**：`tests/` 为空，161 个测试全是模块内 `#[cfg(test)]` 单元测试 | 全库 | A1/A3/C1 这类跨 seam 缺陷**只有集成测试能回归**；可测试性被低估 | 中高 |
| G3 | **`engine.check` 的 Err 被吞**：内存态已改而持久化可能失败→内存/DB 分叉 | `async_monitor.rs:65-69` | engine 内 `set_alerting`/streak 已改内存，若 alert_state/history 写库失败则与 DB 分叉，重启 `with_state` 读旧值→重复/漏报 | 中高 |
| G6 | **Once/Monitor 为二等公民**：无持久化/指标/热更新；Once 硬编码 `interval=5`（Monitor/Server 走 `--interval`） | `main.rs:83` 等 | 模式一致性缺口；结果编排三模式各写一遍（与 D8/E 呼应） | 中 |

> （G5 为对 B2 的严重度校准，已并入上方 B2 行；G4 已拆为 F 簇。）

## 有意取舍（Decision：不实现，仅在后续 ADR / 部署文档显式声明）

以下项对**单实例自托管**定位属非目标或投机性泛化，评审一致建议**不实现**，改为记录为有意约束，避免被当作待办债务反复提起：

- **C8（告警多实例协调）/ D11（会话与指标水平扩展）**：声明"单实例部署"为架构约束；若未来确有横向扩展诉求，另起 ADR 评估外部会话存储与指标聚合。
- **B12（仓库 trait 抽象）**：存储后端钉死 SQLite 是有意取舍（测试已跑真实 SQLite）。
- **D9（API 版本化）**：唯一消费者是同一二进制的控制台，无需 `/api/v1`。
- **A5（增量调度）**：成本高、回报低；全量 reload 的热态重置可接受。先做 A3 的原子切换即可。
- **E4（空详情变体合并）/ D7（指标库替换）**：低优，仅当 D1 之后确有需求再评估（D7 若采纳将取代 D1）。

## 治理方案自身的新风险（R 组，实施时必须规避）

| 编号 | 针对 | 新风险 | 缓解 |
| --- | --- | --- | --- |
| R1 | A1/A2 supervisor | 无脑重启→重启风暴、掩盖根因 | 指数退避 + 熔断（连续失败 N 次置 Failed 并上报指标/历史）；JoinError 作为一等可观测事件 |
| R2 | A3 build-then-swap | 双份运行窗口：同一 monitor_id 短时两个任务并发探测、两个 `AlertsEngine` 并发写同一 alert_state 行（接 C8 竞态） | 保证"旧 abort 完成后再 start 新"，不要全集并存；或按 id 增量切换 |
| R3 | D2 统一信封 | 直接冲击前端 `static/index.html` 的 `j.code`/`r.ok` 混用判断 | 前后端**同批次**改；错误形状变更需同步控制台 |
| R4 | C5 去抖持久化 | 每轮 UPSERT 写放大，加剧 B8/A1 写压与 WAL | 优先声明"去抖进程内瞬时语义"为取舍；若持久化则写合并/只在阈值边界附近落库 |
| R5 | A1 try_send 丢弃 | 把阻塞换成**静默丢数据** | 明确"丢弃只影响落库/CSV/指标、不影响告警链（告警已在任务内完成）"，且**必须计数打点** |
| R6 | B1 RepoError 收口 | 面大、易出现两种错误类型并存的半改混乱期 | 自下而上：先在仓库边界加 `From` 转换保留内层，逐层上移 |

## 补充决策项（第二轮评审要求补记）

- **测试策略**：承认 `tests/` 为空；建立 **seam 集成测试 + 命令类监控可注入（E3）**，作为其余各批修复的**回归门槛**（G2）。
- **跨模式一致性**：Once/Monitor/Server 在告警语义、默认值、编排上对齐；G1「Once 下去抖永不触发」要么修、要么显式记为取舍（G6）。
- **错误传播闭环**：`engine.check` 的 Err 不再被吞（G3），与 B1/C6 合并为"错误进入 结果/历史/指标，而非仅日志"的统一决策。
- **ADR 关系与回归防护**：每批触碰 0002~0006 的改动，在新 ADR「相关」栏显式写 `深化/修订/取代 ADR-000X` 并在旧 ADR「风险与跟进」回链；并发语义类改动合并前须补针对性测试。

## 决策（Decision）：收敛后的分批执行序

优先级 = 严重度 × 触发概率 × 爆炸半径，并受"单文件单 Agent""B1 前置""D1 与 A6 合并""A3 先于 A5""告警内部严格串行"等依赖硬约束。**main.rs 与 engine.rs+notify.rs 是两大多 Agent 踩踏热点，同批内各须单一 owner。**

- **Batch 0 · 低风险快赢（文件互不相交，可真并行）**
  内容：B7+B8（`result_repo.rs` 分块查询/分批删除）、B13（迁移：name 非空或 partial unique）、C10（smtp 缓存上限+口令哈希键）、D10（新增免鉴权 `/readyz`）、C11（webhook client expect→Option）、E5（ID 一致性，若便宜）。
  验证：build + 各自单测。风险：低。深化 ADR-0005（B8）、对齐 ADR-0003（C11）。

- **Batch 1 · 便宜的 outage 收口（main.rs/scheduler，小改动）**
  内容：**A6+D1 合并**（去 metrics 外层锁 + 后台锁中毒恢复）、**A3**（reload build-then-swap，含 R2 规避）、**D6**（CSV 清理或默认关闭）、**D5**（import 走 `validate_config`）。
  验证：build + 新增（reload 遇 DB 失败保留旧任务、metrics 无死锁、CSV 清理）。风险：中。深化 ADR-0002/0006，修订 ADR-0002（D6）。

- **Batch 2 · 运行时大改（专题，先冻结接口契约再 DISJOINT 拆：α=main.rs/β=scheduler/γ=async_monitor）**
  内容：**A1+A2+A4**（try_send 背压+丢弃计数（R5）、任务监督+退避熔断（R1）、关停 drain）、**D3+D4**（集中 Config + AppState/newtype）、**G3**（错误不再吞）。
  验证：build + 通道满丢弃计数、任务 panic 退避重启、关停 drain。风险：高（并发语义）。深化 ADR-0002/0003/0004。

- **Batch 3 · 错误模型 + 契约 + 配置写入路径**
  顺序：**B1 先**（RepoError，含 run_blocking 透传，R6 自下而上）→ 然后两组互不冲突 sweep：组一 **D2**（统一 ResponseError 信封，前端同批 R3）；组二 **B5→B2+D5 收尾**（统一规范化→配置单一真相源）。**B4**（事务边界）随 B1 之后在 repo 层做。注意 `monitor_repo` 被 B1 与 B2 都碰，**B1 先完再 B2**。
  验证：build + 契约形状测、配置往返一致测、事务一致性测。风险：高（横向）。深化 ADR-0002/0006。

- **Batch 4 · 告警子系统重构（`engine.rs`+`notify.rs` 串行单 Agent）**
  3a：C3（Channel trait）+ C4（结构化 `AlertEvent`）。
  3b：C1+C6+C7（送达结果回写状态机 / 历史与送达解耦 / 统一重试归属）。
  3c：C2+C9（按规则分桶抑制/去抖 + 历史记 rule_type；带 alert_state/history 迁移）；C5 按取舍声明或谨慎持久化（R4）。
  同时处理 G1（Once 跨模式告警一致性）。C8 仅声明取舍。
  验证：build + 送达失败不抑制→重发、恢复、多规则独立。风险：高（状态机）。深化 ADR-0004。

- **Batch 5 · 监控抽象扇出 + 迁移/收尾**
  内容：**E1+E2**（冻结 trait 签名→12 monitor 并行 conforming→`domain/check`）、E3（CommandRunner 抽象）、B9/B10（newtype、读模型去 Insertable、时间 NOT NULL+去 updated_at 触发器）、B11（Cursor/Page<T>）、B3（防腐层，与 E2/D2 配合）、G6（模式一致性收尾）、F1~F4（依赖/构建卫生）。D7 若采纳在此取代 D1。
  验证：build + 各监控单测 + CommandRunner 注入 fake。风险：E1 面广但机械 DISJOINT；B10 带迁移需 up/down 可逆。深化 ADR-0003。

- **贯穿全程**：按「补充决策项」建立集成测试（G2），作为每批回归门槛。

## 实施计划（可并行的子任务）

- [x] 子任务 1 — `[独立]` 四分区只读审查（第一轮）。
- [x] 子任务 2 — `[依赖 子任务 1]` 汇总、去重、交叉确认、分簇分级。
- [x] 子任务 3 — `[独立]` 四角度评审讨论（第二轮）+ 校准/补充/风险/收敛排期回填本文件。
- [ ] 子任务 4 — `[依赖 子任务 3]` 按 Batch 0→5 依次创建 ADR-0008+ 并多 Agent 实现，每批集中 `cargo build`/`test` 并回填。

## 后果（Consequences）

### 正面
- 架构债务从"散落在代码/脑中"变为**可见、可排期、可追溯**的台账；经两轮多 Agent 交叉评审，严重度已按真实部署定位校准，排期含依赖约束、冲突面与回归门槛，可直接驱动实现。
- 明确区分了"必须改 / 可改可不改 / 不建议改（有意取舍）"，避免在非目标项上过度设计。

### 负面 / 代价
- 清单规模大（A~G 共约 50 条）；必须按批、每批独立 ADR + 测试，推进较慢。
- 多数批次会触碰已接受的 ADR-0002~0006，需在新 ADR 显式标注深化/修订/取代关系，维护成本上升。
- 在建立集成测试（G2）之前，Batch 1/2 的并发语义类改动回归风险偏高——这也是把测试策略列为贯穿项的原因。

### 风险与跟进
- 实施时须规避 R1~R6 列出的新风险（尤其 R2 双份运行窗口、R3 前端兼容、R5 丢弃计数）。
- 多实例相关项（C8、D11）与存储抽象（B12）、API 版本化（D9）、增量调度（A5）已定为**有意取舍**；若未来需求变化须另起 ADR 推翻本决策。
- 审查均为**静态只读**分析，个别行号以当前工作区为准，实施前以实际代码复核。
