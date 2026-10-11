# ADR-0008：Batch 2 运行时可靠性——结果背压解耦、任务监督与协作式关停

- 状态：已接受
- 日期：2026-10-11
- 决策者：项目维护者
- 相关：深化 [ADR-0002](<0002-unify-async-threading-model.md>)（线程/通道模型）、[ADR-0003](<0003-performance-optimizations.md>)（每实例热态）、[ADR-0004](<0004-timeout-and-alert-semantics.md>)（采样节流/告警语义）；落实 [ADR-0007](<0007-architecture-design-flaws-review.md>) 的 Batch 2（A1/A2/A4、D3/D4、G3）

## 背景（Context）

ADR-0007 盘点出运行时可靠性簇的高危项。本 ADR 落实其 Batch 2：先冻结接口契约，再按 `main.rs / scheduler.rs / async_monitor.rs` 单文件单改动点实现，配针对性回归测试，不改前端。

## 决策（Decision）

### A1 · 结果发送改 try_send + 丢弃计数（深化 ADR-0002/0004）
监控任务此前 `tx.send().await`：通道满时**阻塞探测**，把慢消费者的背压传导成"所有探测卡住"（单点耦合）。改为 `try_send`：
- 通道满（`Full`）：**丢弃本条结果**、累加进程级丢弃计数并限频告警，**继续探测**，不阻塞。
- 通道关闭（`Closed`）：消费端已退出，任务结束。
- 丢弃语义（R5）：告警评估已在任务内 `engine.check` 完成（发送之前），**丢弃只影响落库/CSV/指标，不影响告警链**；丢弃必须计数并经 `/metrics` 暴露（`network_monitor_results_dropped_total`），不做"看不见的数据缺口"。

### A2 · 任务监督：捕获 panic + 退避重试 + 熔断（规避 R1）
`check` 内部可能 panic（如锁中毒后的 `.lock().unwrap()`）。此前 panic 直接杀死该监控任务、静默停产而列表仍显示存在。现在：
- 每轮 `check` 放到子任务执行并 `await` 其 `JoinHandle`，用 `JoinError` **捕获 panic**（无需引入 futures crate、无需 UnwindSafe 约束）。
- panic 时：累加 `network_monitor_task_panics_total`、记 error，按**指数退避**（1s→上限 60s）后重试，不立即重启形成风暴（R1）。
- **熔断**：连续 panic 达到上限（默认 5 次）则记 error 并**停止该任务**（置为失败而非无限重启），避免持续 panic 刷日志吃 CPU；成功一轮即清零连续计数。

### A4 · 协作式关停 + 兜底 abort（ADR-0007 A4，评审降级后取轻量实现）
关停不再对探测侧纯硬 `abort`：调度器持有 `Arc<AtomicBool>` 停止标志，任务在循环顶检查、置位后在**下一个循环顶干净退出**；`shutdown` 置位后给一个短暂宽限期（默认 500ms）让任务自然退出，再对仍存活的句柄 `abort` 兜底。在途单轮探测尽量跑完，减少"优雅关停仍硬砍在途探测"。

### G3 · 告警评估错误不再静默吞（部分，深化留待 Batch 4）
`engine.check` 返回 Err 时此前仅 `tracing::error!`。本批补充累加 `network_monitor_alert_eval_errors_total` 使其可观测；内存态/持久化分叉的**根治**（错误进入历史/状态机闭环）依赖告警状态机重构，归 Batch 4（C1/C6）。

### D3 · 集中配置（修订 BIND_ADDR 双读漂移）
新增 `config::AppConfig`，启动时一次性读取 `BIND_ADDR / CSV_PATH / RESULT_RETENTION_DAYS / LOG_DIR / 默认间隔` 等运行配置。`auth` 不再自行二次读 `BIND_ADDR` 推断 cookie `Secure`，改由装配层把**同一个** bind 值注入 `AuthConfig`——消除"auth 推断的 bind 与 main 实际 bind 背离"的潜在漂移（ADR-0007 D3）。安全敏感的 `ADMIN_* / METRICS_TOKEN` 仍由 auth 读取，不扩大本批面。

### D4 · default_interval newtype（去基元键 DI 脆弱）
`web::Data<u64>` 以裸 `u64` 作 DI 键，易与其它 `u64` 注入混淆、装配错配到运行期才暴露。引入 `DefaultInterval(u64)` newtype 作为注入类型，handler 显式提取，装配错配变为编译期可查。

## 契约（实现前冻结）

- 结果信封 `MonitorResultMessage`、通道容量(100)不变；仅发送方式 send→try_send。
- `AsyncMonitor::create_interval_monitoring` 新增入参：`stop: Arc<AtomicBool>`（协作式关停）。
- `Scheduler` 持有 `stop: Arc<AtomicBool>`，`shutdown` 为 async（含宽限期）。
- 新增计数器均为进程级 `AtomicU64`，由 `MetricsRegistry::render` 追加输出，Once/Monitor 模式不受影响（/metrics 仅 Server 模式暴露）。

## 后果（Consequences）

### 正面
- 慢消费者不再卡死全体探测；单监控 panic 不再静默停产且不形成重启风暴；关停更干净；丢弃/ panic/告警错误均可观测。
- 配置漂移与基元键 DI 风险在装配层收口。

### 负面 / 代价
- 每轮 check 多一次 `spawn`+`await`（秒级间隔下开销可忽略）。
- 熔断停止的任务当前仅靠日志/指标暴露，尚无 UI 失败态（留待后续）。
- G3 仅做到可观测，未闭环；A4 为协作式+兜底，非严格零丢失。

### 风险与跟进
- R1（重启风暴）：以指数退避+熔断规避。R5（静默丢数据）：以丢弃计数+ /metrics 暴露规避。
- 深层 G3、任务失败态 UI、按 id 增量调度（A5）留待后续批次/ADR。
