# ADR-0007：架构级设计缺陷盘点与分批治理方向

- 状态：草案（待确认后置为「已接受」）
- 日期：2026-10-11
- 决策者：项目维护者
- 相关：[ADR-0001](<0001-adopt-adr-and-multi-agent-workflow.md>)（多 Agent 工作流）、[ADR-0002](<0002-unify-async-threading-model.md>)~[ADR-0006](<0006-security-hardening.md>)（已记录的性能/安全改造）

## 背景（Context）

ADR-0002~0006 聚焦**性能热路径与安全加固**，并已实现验证。应「重新分析设计缺陷」的要求，本次用「多 Agent 开发模式」对代码库做了一次**架构级**复盘：按 4 个分区各派一个只读审查 Agent（监控引擎/调度、告警子系统、数据库/领域、API/鉴权/配置/可观测性），要求只报告 ADR-0002~0006 **之外**的、架构/设计层面（而非代码风格）的缺陷，每条给出 位置、缺陷、影响、严重度、建议方向。

本 ADR 的定位：**作为权威的缺陷清单（inventory）与分批治理排期**，不在本条内直接实现。按 ADR 工作流，具体修复将按簇另起 ADR（0008+）逐簇决策与实现。

说明标记：
- 严重度：高 / 中 / 低（多规则、多实例等条件下会升级的，注明「条件趋高」）。
- `★`：由 ADR-0002~0006 的改造**引入或遗留**、与其意图相悖的新债务。
- `⊕`：被 ≥2 个分区审查**交叉确认**，置信度更高。

## 缺陷清单（Decision 的事实依据）

### A 簇 · 运行时监督、任务生命周期与可靠性

| 编号 | 缺陷 | 位置 | 影响 | 严重度 |
| --- | --- | --- | --- | --- |
| A1 | **单消费者 + 单通道是全局单点**：所有监控任务 `send().await` 到唯一 bounded(100) 通道、唯一 `consume_results` 消费 | `main.rs:188`、`main.rs:311-365`、`async_monitor.rs:74` | 消费者 panic→所有 send 返回 Err→任务逐个退出、监控全盘静默消失；消费者变慢→通道灌满→所有探测阻塞在 send、配合 `Skip` 丢采样。互不相关的监控被耦合成命运共同体 | 高 |
| A2 | **任务无监督、无重启**：spawn 的 interval 任务 panic 后，JoinHandle 仍留在 `Scheduler.tasks`，无人观测 JoinError | `async_monitor.rs:46`；各 `check` 内 `.lock().unwrap()`（`cpu.rs:36/45/49`、`disk.rs:32`、`memory.rs:26`、`process.rs:26`、`http.rs:49`） | 单个监控永久静默停产，而 `active_task_count` 仍计数、列表仍显示"存在" | 高 |
| A3 | **热更新非原子**：`reload_all` 先 `abort` 全部任务，**之后**才读库；DB 读失败即 `return` | `scheduler.rs:45-64` | 旧任务已毁、新任务未建→一次瞬时 DB 抖动使全量监控清零且不自愈。破坏性操作先于"替代可用"确认 | 高 |
| A6 ⊕★ | **后台任务仍以 `.expect("锁中毒")` panic**：消费循环与会话清扫未纳入 ADR-0006 E2 的中毒恢复覆盖面 | `main.rs:339`（消费者 `metrics.lock().expect`）、`main.rs:221`（sweep `session锁中毒`） | 同一把锁 handler 侧恢复、后台侧 `.expect`：handler 持锁 panic 使锁中毒后，消费者下次 `record()` panic→触发 A1 全盘停摆，且进程不退出（探活仍绿），故障隐蔽 | 高 |
| A4 | **关停对探测侧非优雅**：优雅关停只体现为"等消费者攒批落库"，监控任务被 `abort()` 硬砍 | `scheduler.rs:76-81`、`main.rs:269-276` | 在途探测（含已 spawn 的 ping/tracert 子进程、在途 send）被硬取消、结果丢失。"优雅关停"名实不符 | 中 |
| A5 ★ | **全量重建重置无关监控热态**：改任一条配置都 abort+重建**所有**任务 | `scheduler.rs:45-62` | ADR-0003 刚引入的每实例复用状态（CPU 预热基线、DNS resolver、HTTP 正则缓存）随之清零；无辜监控被打断、CPU 下轮重新预热 | 中 |

### B 簇 · 领域模型、存储边界与一致性

| 编号 | 缺陷 | 位置 | 影响 | 严重度 |
| --- | --- | --- | --- | --- |
| B1 ⊕ | **全项目缺领域错误类型**：`diesel::result::Error` 贯穿全栈外泄；`pool` 把"连接池耗尽/超时"硬塞进 `QueryBuilderError`；工具层用裸 `Box<dyn Error>`（非 Send/Sync）+ 字符串错误 | `pool.rs:96-101`、`response.rs:41-62`（`run_blocking` 丢弃错误类型）、`tools/http.rs:54-57` | 唯一冲突/NotFound/池耗尽(可重试)/查询 Bug 全坍缩成不透明枚举，上层无法分类/重试/告警；需识别 409 的 handler 被迫绕开 helper 重写样板 | 高 |
| B2 | **监控配置双重真相**：整份 `MonitorDefinition` 序列化进 `config_json`，同时 target/method/interval_ms/timeout_ms/monitor_type/tag 又作独立列落库；更新路径只改列不同步 blob | `services/mod.rs:25-36`、`models.rs:41-51`、`MonitorConfigUpdate`/`set_enabled`、`scheduler.rs:86`（按 config_json 重建） | 列与 blob 无一致性约束→"界面显示已改、调度器仍按旧 config_json 监控旧目标"的分叉 | 高 |
| B3 | **缺防腐层**：diesel `Queryable` 结构体直接派生 `Serialize/Deserialize`，DB 列布局=对外 JSON 契约；只有领域→入库映射，无存储→领域方向，`CheckResult` 落库被压扁后不回构 | `models.rs:7-8/53` | DB schema 演进直接破坏 API 契约；领域富类型信息在往返中丢失 | 中高 |
| B4 | **全栈无事务边界**：无一处 `conn.transaction` | `monitor_repo.rs:43-48`（count+list 两段查询）、alert_state 写 + alert_history 写各自独立 `get_connection` | 并发写下"总数 11 却只翻出 10 条"；跨表状态/历史中途崩溃不一致 | 中高 |
| B5 ⊕ | **同一配置两条并行转换 + interval 单位漂移**：`build_monitor_insert` 与 `MonitorConfig::from_entry` 各自推导默认值；一处 `*1000` 存毫秒、一处按秒；且 `timeout.max(1000)` 下限转换在 5+ 监控里逐个手写 | `services/mod.rs:10-37`、`check.rs:26-156/151`、`tcp.rs:36`/`icmp.rs:71`/`dns.rs:50`/`udp.rs:54`/`ftp.rs:43` | 默认值/单位漂移风险；超时策略变更需遍历每个监控 | 中 |
| B6 ⊕ | **Service 层空壳 + 分层不一致**：monitor/result 走 service（近乎一行透传），alert/status/metrics 在 handler 内直接从 pool new 仓储 | `monitor_service.rs:17-18`、`alert_state_repo.rs:8`、`alert_handlers.rs:30-33`、`status_handlers.rs:49`、`metrics_handlers.rs:19` | 持久化边界被击穿；事务/不变量/错误翻译无处安放；两种数据访问风格并存 | 中 |
| B7 | **`get_latest_by_monitor` 用无界 IN 列表**（参数数=监控数） | `result_repo.rs:72-84` | 监控数增大撞 `SQLITE_MAX_VARIABLE_NUMBER` 直接报错 | 中 |
| B8 ⊕★ | **保留策略单条无界 DELETE** 作用在最热大表 | `result_repo.rs:28-34` | 长事务独占写锁阻塞结果写入并撑大 WAL，与 ADR-0005 控 WAL 意图相抵（应分批删除） | 中 |
| B9 | **领域不变量裸 i32/String 无 newtype**；读模型 `CheckResultModel` 误标 `Insertable` 且含 id/时间戳 | `models.rs:53-64` | enabled/alerting/status 为 i32、monitor_type 读回 String；类型上允许绕过 AUTOINCREMENT/CURRENT_TIMESTAMP 插整行 | 中 |
| B10 | **时间表示脆弱**：裸 `Option<NaiveDateTime>` 无时区、"存储即 UTC"是散落的隐式约定；列可空却恒有 DEFAULT→幽灵 Option；只增不改的 `check_result` 仍带 `updated_at` 列+触发器 | `result_repo.rs:30-31`、`up.sql:14-15/49-54` | 时区混淆风险；Option 污染全调用点；热表冗余触发器 | 中 |
| B11 | **分页契约碎片化**：OFFSET+COUNT 与 keyset 并存，游标为裸 `Option<i32>`，无 `Cursor`/`Page<T>` 封装；monitor_config 仍只有 OFFSET | `monitor_repo.rs`、`result_repo.rs`、`response.rs:14-37` | 接口层分页语义不统一，调用方需按端点分别处理 | 低 |
| B12 | **仓库硬绑 `Arc<SqlitePool>` 无 trait 抽象** | `database/repositories/*` | 上层无法脱离真实 SQLite 测试，存储后端被钉死 | 低 |
| B13 | **`monitor_config.name` UNIQUE 建在可空列** | `up.sql:5` | SQLite 多 NULL 互异→可插多条无名记录使去重静默失效 | 低 |

### C 簇 · 告警子系统语义

| 编号 | 缺陷 | 位置 | 影响 | 严重度 |
| --- | --- | --- | --- | --- |
| C1 | **EMAIL 乐观 Delivered 静默丢告警**：抢到信号量许可即整单后台化并无条件返回 `Delivered`，后台重试全败仅打一行 error | `notify.rs:127-157`、`engine.rs:156-167` | `alerting=true` 据此置位并抑制同一故障后续重发；SMTP 持续故障时用户完全收不到告警且系统自认已告警。ADR-0004 只修了"许可耗尽"支，"抢到许可但重试全败"这一常见支仍漏报 | 高 |
| C2 | **单一 `alerting` 位覆盖一组异构规则 + evaluate 首命中即返回**：多条规则（可用性/阈值/响应码/内容）共用同一抑制位与同一套去抖计数 | `engine.rs:30`、`engine.rs:209-255` | 规则互相掩盖（DOWN 告警后 CPU 阈值越限不再通知）；多个独立故障被折叠成一次 triggered/recovered，历史无法区分规则 | 中（多规则下趋高） |
| C3 | **通知渠道硬编码巨型 match，`AlertSender` 抽象名存实亡**：webhook/EMAIL 两路各自重复"信号量→spawn→退避重试→permit→日志"整套样板 | `notify.rs:85-216/114-157 vs 253-295`、`domain/alert.rs:142-163` | 新增渠道要同时改枚举+parse+match+校验多处，违背开闭；渠道不可单测、不能按渠道配独立重试 | 中 |
| C4 | **告警事件过早坍塌为展示字符串**：规则判定直接产出中文纯文本，engine↔notify 以 `String` 为契约，"恢复 vs 告警"靠 `message.starts_with(前缀)` 嗅探 | `engine.rs:157/181-188`、`notify.rs:106` | 渠道拿不到结构化字段，无法渲染富格式；语义靠字符串前缀传递脆弱 | 中 |
| C5 | **半持久化状态机**：`alerting` 从 alert_state 恢复，但去抖计数 hit_streak/ok_streak 仅内存 | `engine.rs:31-37/90-98`、`scheduler.rs` 每次 reload `with_state` 重建 | 进程重启/热更新时去抖窗口断裂，告警被额外延迟 `N×interval`（恢复侧同理） | 中 |
| C6 | **alert_history 仅在同步 Delivered 时写入**：后台重试送达/Deferred/最终失败均不入史 | `engine.rs:162-167/194-199` | 历史表无法反映"是否/何时/经由哪次尝试送达"，通知失败对控制台不可见 | 中 |
| C7 | **两套重试机制重叠**：notify 后台退避 + engine"下轮重发" | `engine.rs:171`、`notify.rs:267-295` | 抖动时两条恢复路径可能都成功→重复告警；重试无单一事实来源 | 中 |
| C8 | **多实例无协调**：alert_state 以 monitor_id 单行存，无 owner/租约/版本/锁 | `alert_state_repo.rs:40-57`、`async_monitor.rs:36-37`（注释假定独占） | 多实例/误部署两份→重复告警风暴 + upsert 相互覆盖；水平扩展硬上限且无护栏 | 中 |
| C9 | **NotifyCondition 积类型大杂烩 + alert_history.alert_type 实为 monitor_type** | `domain/alert.rs:18-31`、`engine.rs:163/195` | 非法规则组合只能靠分散的运行时守卫；历史 `alert_type` 名实不符，回溯时得不到触发规则 | 中 |
| C10 | **SMTP transport 缓存无界、键含明文口令、无 TTL/驱逐** | `tools/smtp.rs:24-33/75-90` | 口令轮换/多配置使缓存只增不减→内存泄漏；明文口令长期驻留 | 低 |
| C11 ★ | **webhook_client 构建失败 `expect` panic**（ADR-0003 已修 HttpMonitor 同类，此处遗留） | `notify.rs:33-42` | 首个 webhook 告警触发时若 Client 构建失败则 panic，与"不在运行路径 panic"范式相悖 | 低 |

### D 簇 · API 层、启动装配与可观测性

| 编号 | 缺陷 | 位置 | 影响 | 严重度 |
| --- | --- | --- | --- | --- |
| D1 | **MetricsRegistry 双重加锁**：内部已有 `Mutex` 自同步、方法全 `&self`，装配层又包进 `Arc<Mutex<MetricsRegistry>>` | `metrics.rs:58-60`、`main.rs:183`、`metrics_handlers.rs:24-27` | 两把锁保护同一状态，恒冗余、徒增争用，且两层中毒恢复策略不一致（接 A6） | 高 |
| D2 | **统一响应信封名不副实**：成功走 `{code,message,data}`，400/404/500/504 回退 actix 纯文本，401/403/409 又各自手写 JSON | `response.rs:41-44`、`validation.rs`、`monitor_handlers.rs:134/190-198`、`auth.rs:426-439` | 同一 API ≥3 种响应形状；body 内 `code` 与 HTTP 状态码重复承载、错误路径形同虚设；前端无法统一解析 | 高 |
| D3 | **配置来源分散、无集中 Config**：十余项 env 散落各处；`BIND_ADDR` 被 auth（推断 cookie Secure）与 main（实际 bind）两处各自读取 | `auth.rs:84-118/107`、`main.rs:228`、`logging.rs:22`、`args.rs:20-27` | bind 解析方式一旦演进，Secure 推断会与真实 bind 悄然背离→生产下发非 Secure cookie 而无告警；配置无法集中校验/文档化/测试注入 | 高 |
| D4 | **类型键式 DI 脆弱、无 AppState**：8 个独立 `app_data`；`web::Data<u64>` 用裸基元作键；包裹层数错误能编译却运行期 500（注释自承） | `main.rs:232-244` | 装配错误只在运行时表现为 500；每个 handler 测试需手工重注子集，可测试性差 | 中 |
| D5 | **校验未集中**：`validate_config` 仅在 API 创建/更新路径调用，`import_monitor_list` 启动导入全程不校验 | `validation.rs:11`、`monitor_handlers.rs:165/221`、`main.rs:427-446` | interval=0、非法正则、未知渠道经 JSON 文件可直接入库，运行期才暴露 | 中 |
| D6 | **CSV 与 DB 治理不对称**：DB 有 `RESULT_RETENTION_DAYS` 清理，CSV 按日生成文件后永不回收 | `csv_logger.rs:45-56`、`main.rs:158-180` | 长期运行 CSV 目录无界增长撑满磁盘，反过来拖垮本应权威的 DB 写入 | 中 |
| D7 ⊕ | **自实现 Prometheus 注册表 + 单一全局写锁**：手拼 exposition 文本，单 `Mutex<HashMap>` 串行所有监控的 record 与 scrape | `metrics.rs` 全文、`metrics.rs:58-60` | exposition 规范需人工维护（无 OpenMetrics/内容协商）；监控数增大后单锁成写入热点。ADR-0002 A3 只优化了 render 持锁时长 | 中 |
| D8 ⊕ | **run_monitor_once 在 handler 内重复消费循环编排**：手动执行把"更新指标+持久化"复制进 API 层 | `monitor_handlers.rs:364-455`、`main.rs:333-354`、`async_monitor.rs:57-63/93-99`（信封组装重复） | 两份逻辑易漂移（字段换算、落库时机、指标标签），重编排堆在 handler 难单测 | 中 |
| D9 | **缺少 API 版本化** | `api/mod.rs:22-23`（`/api` 无 `/v1`） | 响应信封演进（D2）将直接破坏既有调用方 | 低 |
| D10 | **无就绪探针**：liveness 恒 200 不查依赖（合理），但无免鉴权 readiness，`/api/status` 又需会话 | `health_handlers.rs:1-10`、`auth.rs:342-353` | DB 连接池耗尽时 `/healthz` 仍绿，编排器不摘流/重启而请求已失败 | 低 |
| D11 | **内存会话 + 内存指标无法水平扩展**（已承认单管理员定位但未进 ADR） | `auth.rs:1-7/181-187`、`metrics.rs` | 限定单实例部署；多实例下会话需粘滞、各实例指标互不一致（与 C8 呼应） | 低 |

### E 簇 · 监控抽象与类型（以 HTTP 为中心的渗漏）

| 编号 | 缺陷 | 位置 | 影响 | 严重度 |
| --- | --- | --- | --- | --- |
| E1 | **`Monitor` trait 抽象弱**：`(bool, CheckResultDetail)` 二值返回语义跨实现漂移（HTTP 连接失败返回 true、Unknown 恒 false、TCP 恒 true），bool 兼表"执行失败"与"目标不可用"，无类型化错误 | `monitor/mod.rs:14`、`http.rs:439/462/503/512`、`tcp.rs:21/43`、`unknown.rs:26` | 调用方拿 bool 直接当 `status` 落库，语义不统一；渗漏抽象 | 中 |
| E2 | **结果模型 HTTP 中心化**：CPU/Memory/Disk/Process 走兜底 `(status, 0, None)`，真实度量不投影到通用字段，`status: bool` 无 degraded/unknown | `domain/check.rs:242-265` | 对资源类监控表达力为零；对 CPU% 做阈值告警必须深挖 detail | 中 |
| E3 | **命令类监控硬编码外部二进制**：直接 spawn `ping`/`tracert`/`traceroute` 并解析跨 locale stdout | `icmp.rs:91-106/25-58`、`traceroute.rs:36-74` | 无命令执行抽象→不可注入测试，强依赖目标机二进制与系统语言 | 中 |
| E4 | **`MonitorConfigDetail` 11 个空变体 + 工厂与详情双真相源**：类型已在工厂定好，各 `check` 又重新 match `config.details` 不匹配回退 Unknown | `domain/check.rs:161-206`、`monitor/mod.rs:53-68`、`http.rs:446`、`unknown.rs:20` | 同一"类型→行为"用两套结构表达可能漂移；大量空变体纯样板 | 低中 |
| E5 | **ID 模型不一致**：`CheckResult.id` 为 u128（uuid），内容校验 rule_id 截断为 u64 | `async_monitor.rs:58/95`、`http.rs:259`、`main.rs:457` | 跨 CSV/DB 关联的 ID 类型表现不一，建模一致性问题 | 低 |

## 跨模块共性主题（根因）

1. **缺失"监督/隔离"这一架构层**：所有 `spawn` 任务（监控、消费者、清扫、保留清理）句柄或存或弃，无人观测 `JoinError`、无重启、panic 不隔离（A1/A2/A6 同根）。整个运行时缺一个 supervisor 概念。
2. **破坏性操作缺乏事务性/原子性**：reload 先毁后建（A3）、shutdown 硬 abort（A4）、全量重建毁热态（A5）、DB 无事务（B4）——皆"先毁再建"而非"建成再换"。
3. **抽象边界被选择性击穿或过早坍塌**：service 层旁路（B6）、统一响应信封旁路（D2）、`run_blocking` 丢类型（B1）、告警事件变纯文本（C4）、渠道巨型 match（C3）——主路径遵守、旁路绕过，保障力被稀释。
4. **单一真相源缺位 + 不变量退回运行时兜底**：config_json vs 列（B2）、入库转换 vs 运行时转换（B5）、工厂 vs 详情枚举（E4）；布尔/枚举用裸整数字符串（B9）、时区靠口头约定（B10）、唯一约束建在可空列（B13）。
5. **送达真相与状态机脱节**（告警专属）：`Delivered/Deferred` 这个只反映首发的同步枚举被当作权威送达信号，驱动抑制置位与历史落库（C1/C6/C7），缺"后台最终结果回写状态机"的闭环。
6. **"自己造轮子"的长期成本**：自实现 Prometheus 导出（D7）、CSV/DB 双写治理不对称（D6）——短期零依赖，长期承担规范维护与资源治理负担。
7. **加固只覆盖部分调用路径**：锁中毒恢复（A6）、输入校验（D5）都只在"某些入口"生效，后台任务与 JSON 导入成为绕过口——加固应收口在边界而非逐处补。

## 决策（Decision）

1. **采纳本清单为权威缺陷台账**，优先级 = 严重度 × 触发概率 × 爆炸半径。
2. **分批治理，按簇另起 ADR（0008+）** 决策与实现，不在本条内直接改代码。建议排期：

   - **第一批（高危可靠性，强烈建议优先）**：A6/D1（锁与中毒恢复一次性收口，去掉 metrics 外层锁）、A3（reload build-then-swap）、A1+A2（消费者/任务监督 + send 背压解耦）。
   - **第二批（一致性与边界）**：B1（领域错误类型 `RepoError`/`thiserror`，在仓库边界收口）、B2（配置单一真相源）、B4（事务边界）、D2+D3+D4（统一错误信封 + 集中 `Config` + `AppState`）。
   - **第三批（告警语义）**：C1+C6+C7（送达结果回写状态机、历史与送达解耦）、C2（按规则分桶抑制/去抖）、C3+C4（`Channel` 抽象 + 结构化 `AlertEvent`）。
   - **第四批（规模与治理）**：B7/B8（无界 IN / 分批 DELETE）、D6（CSV 清理或开关）、D7（评估成熟指标库）、E1~E3（trait 返回类型化、结果模型去 HTTP 中心化、命令执行器抽象）。
   - **余项（低优/取舍声明）**：B9~B13、C8~C11、D9~D11、E4/E5——部分可随上述批次顺带处理，部分（C8/D11 多实例、B12 存储抽象）如无多实例诉求则**显式声明为有意取舍并记入对应 ADR**，不强行实现。

3. 每批遵循 ADR 工作流：写 ADR（草案）→确认→拆分 DISJOINT 子任务多 Agent 实现→集中 `cargo build`/`test`→回填。

## 实施计划（可并行的子任务）

> 本 ADR 为"分析+台账"，不产出代码。唯一产物是文档。

- [x] 子任务 1 — `[独立]` 四分区只读审查（已由 4 个子 Agent 完成）。
- [x] 子任务 2 — `[依赖 子任务 1]` 汇总、去重、交叉确认、分簇分级（本文件）。
- [ ] 子任务 3 — `[依赖 子任务 2]` 与维护者确认优先级后，按批次创建 ADR-0008+ 并派发实现。
- [ ] 汇总验证 — 本条无需 `cargo`；仅校对文档内链与 README 索引。

## 后果（Consequences）

### 正面
- 架构债务从"散落在代码/脑中"变为**可见、可排期、可追溯**的台账，后续每批修复有明确依据与验收边界。
- 交叉确认（`⊕`）与新债务标注（`★`）帮助区分"真·高危"与"已知取舍"。

### 负面 / 代价
- 清单规模较大（A6/B13/C11/D11/E5 共约 46 条），若一次性全改风险高；必须按批、每批独立 ADR + 验证，推进较慢。
- 部分修复会触碰已接受的 ADR-0002~0006（如 metrics 锁结构、CSV 策略、超时转换归集），需在新 ADR 中显式标注"深化/修订"关系，避免决策漂移。

### 风险与跟进
- 本条为**草案**，严重度与排期需维护者确认；确认后置为「已接受」。
- 多实例相关项（C8、D11）取决于是否有水平扩展诉求——若维持单实例自托管，应在后续 ADR 中正式记录该约束，而非默认实现分布式协调。
- 审查均为**静态只读**分析，未运行 `cargo`、未对活库验证；个别行号以当前工作区为准，实施前以实际代码复核。
