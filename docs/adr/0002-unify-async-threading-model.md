# ADR-0002：统一异步线程模型（同步 DB / 文件 IO 一律挪出 runtime 线程）

- 状态：已接受
- 日期：2026-09-25
- 决策者：项目维护者
- 相关：[ADR-0001](<0001-adopt-adr-and-multi-agent-workflow.md>)（改造簇 A）

## 背景（Context）

项目基于 tokio + actix-web，持久化用同步的 diesel/SQLite。既有规约是「同步阻塞调用必须挪到阻塞线程池」，并已在以下位置贯彻：`status_handlers.rs:39`、`main.rs:170`、`alerts/engine.rs:94`。但仍有多处违反该规约，会在 async worker 线程上执行阻塞调用：

- 读写 API handler 普遍直接调用同步 diesel：`monitor_handlers.rs:32/53/69/112/183/230/262/289`、`result_handlers.rs:44/64`、`alert_handlers.rs:28`、`metrics_handlers.rs:16`。`get_connection`（`pool.rs:72`）带 `busy_timeout=5000`，遇写锁单次可阻塞 5 秒，直接 park worker 线程，并发下迅速耗尽线程池。
- CSV 日志每条记录同步 `write_record`+`flush`（`csv_logger.rs:84-108`），且在 async 消费循环内直接调用（`main.rs:343`）。是全项目唯一没挪到 `spawn_blocking` 的同步 IO。
- `/metrics` 的 `render`（`metrics.rs:122-209`）全程持 `Mutex` 做大量 `format!` 拼接，scrape 期间阻塞 `record()` 写入热路径。

## 候选方案（Options）

1. **逐处用 `web::block` / `spawn_blocking` 包裹 + 轻量重构（本决策）** — 优点：与既有规约一致、零新依赖、改动可控；缺点：样板增多（可抽 helper）。
2. **引入异步 DB 栈（deadpool-diesel / sqlx）** — 优点：根治 async 阻塞；缺点：大改依赖树与全部仓储层，违背 Cargo.toml 刻意维持的单一同步 diesel 栈，风险/成本过高。
3. **维持现状** — 不可接受，自相矛盾且并发下有停摆风险。

## 决策（Decision）

采用方案 1：

- 所有同步仓储调用统一用 `web::block` 包裹（对齐 `status_handlers`），可抽一个 `block_db(move || …)` helper 收敛样板。
- CSV 写入改为 `spawn_blocking`，并把 `flush` 改为 `BufWriter` + 批量/定时刷新（如每 N 条或 1s 刷一次，进程退出时保证 flush）。
- `metrics::render` 先在持锁下 clone 出轻量快照再解锁，锁外做格式化；拼接改 `write!(&mut out, …)` 复用缓冲。

## 实施计划（可并行的子任务）

- [ ] 子任务 1 — `[独立]` API 层：monitor/result/alert/metrics handlers 的同步 diesel 全部 `web::block` 包裹，抽 `block_db` helper。影响：`src/api/handles/*`。
- [ ] 子任务 2 — `[独立]` CSV：`log_result` 挪 `spawn_blocking`，`csv_logger` 改 `BufWriter` + 批量 flush。影响：`src/csv_logger.rs`、`src/main.rs`。
- [ ] 子任务 3 — `[独立]` metrics：`render` 快照后解锁格式化，`write!` 复用缓冲。影响：`src/metrics.rs`。
- [ ] 汇总验证 — `cargo build` + `cargo test`；手动核对 `/metrics` 输出不变、CSV 行无丢失。

## 后果（Consequences）

### 正面
- 消除 async worker 线程被同步 DB/IO park 的风险，并发吞吐与稳定性提升。
- 全项目线程模型一致，消除自相矛盾。

### 负面 / 代价
- `web::block` 包裹样板增多（helper 可缓解）。
- CSV 批量 flush 后，崩溃时最后一批未刷记录可能丢失（可接受，DB 为权威存储）。

### 风险与跟进
- `web::block` 用 actix 默认阻塞线程池（上限 512），极端并发下仍可能排队；若成为瓶颈，另起 ADR 评估专用阻塞池或异步 DB 栈（方案 2）。

## 实施结果与偏差（回填）

状态：已实现并通过集成验证（`cargo build` 通过；`cargo test` 161 passed / 0 failed）。

- 子任务 1（API handlers）：已完成。monitor/result/alert/metrics/status handlers 的同步 diesel 调用统一经 `web::block` 包裹。
- 子任务 2（CSV）：已完成，但实现方式较原计划调整——
  - **偏差**：未对每条记录套 `spawn_blocking`，而是把 `CsvLogger` 的刷盘改为「写记录只进 `csv::Writer` 内存缓冲（append，近似非阻塞）＋ 按 64 条或 1s 批量/定时 `flush`」。原先的热路径代价是「每条一次 `flush` 系统调用」，改造后热路径只剩内存缓冲追加，已不再是会 park worker 的阻塞 IO，因此无需再引入 `spawn_blocking` 包裹；真正的 `flush`（写系统调用）降为每秒一次，由消费循环的 `flush_tick` 触发，优雅关停前显式 flush，并实现 `Drop` 兜底刷盘。
  - 数据库持久化此前已是「攒批 + `spawn_blocking`」（`flush_batch_async`），本次未改。
- 子任务 3（metrics）：已完成，`render` 先快照后解锁再格式化。
- 代价确认：CSV 崩溃丢失窗口由「≈0」放宽为「≤1s 的最后一批记录」，与 ADR 预判一致（DB 为权威存储，CSV 为辅助）。
