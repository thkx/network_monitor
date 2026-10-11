# ADR-0005：数据库分页与连接/PRAGMA 治理

- 状态：已接受
- 日期：2026-09-25
- 决策者：项目维护者
- 相关：[ADR-0001](<0001-adopt-adr-and-multi-agent-workflow.md>)（改造簇 D）

## 背景（Context）

数据库层存在性能与可靠性隐患：

- 控制台结果列表默认走 `COUNT(*)` + OFFSET 深分页（`result_repo.rs:103-108`，由 `result_handlers.rs:63-66` 在未带 cursor 时触发）。`COUNT(*)` 对高写入的 check_result（百万行后）每次翻页近全表计数；OFFSET 深页退化为近全表扫描。keyset 已实现（`result_repo.rs:118`）却仅在显式 `cursor=true` 时启用。
- 连接级 PRAGMA 的错误被吞掉：`let _ = conn.batch_execute("PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 5000;")`（`pool.rs:72`）。若失败则该连接上 `ON DELETE CASCADE` 静默失效，`delete_monitor`（`monitor_repo.rs:120`）会留孤儿行，且无日志。该 PRAGMA 每次借连接都重跑。
- WAL 下未设 `synchronous`（`pool.rs:34`），默认仍 FULL，高频写入 fsync 偏重；未设 `wal_autocheckpoint`。
- 复合索引 `(monitor_id, created_at)`（`migrations/2026-09-17-.../up.sql:5`）疑似无读取使用（所有查询按 `id` 排序，走 `idx_check_result_monitor_id`），仅增最热写表的写放大。
- 连接池 `max_size=8` 无 `min_idle`（`pool.rs:24-26`），未基于「读并发 + 单写者」设定。

## 候选方案（Options）

1. **分页默认 keyset + PRAGMA 建连回调 + pragma 调优 + 索引核实（本决策）** — 优点：对症、风险可控；缺点：`synchronous=NORMAL` 需业务确认持久性权衡。
2. **维持现状** — 深分页与孤儿行风险持续。

## 决策（Decision）

- **分页**：控制台默认改走 keyset；如需总数，仅首页取一次 `total` 或用缓存/近似计数，后续翻页不再 `COUNT(*)`。保留 OFFSET 仅供需随机跳页场景。
- **PRAGMA**：改用 r2d2 `CustomizeConnection::on_acquire` 在建连时设置 `foreign_keys`/`busy_timeout`，失败作为取连接错误上报（并记日志），不再 `let _ =` 吞错，也不再每次借连接重跑。
- **pragma 调优**：✅**已确认**——设 `synchronous = NORMAL`（WAL 下断电最多丢最后几条、不损坏库），并设 `wal_autocheckpoint`。
- **索引**：用 `EXPLAIN QUERY PLAN` 核实后，若复合索引确无读取使用，则删除或改成 `(monitor_id, id)` 显式服务 keyset（二选一，避免与 monitor_id 单列索引重叠）；同步修正 `result_repo.rs:60` 的失实注释。
- **连接池**：基于「读并发 + 1 写」评估 `max_size` 并设合理 `min_idle`。

## 实施计划（可并行的子任务）

- [ ] 子任务 1 — `[独立]` 控制台默认切 keyset；total 仅首页/缓存。影响：`src/api/handles/result_handlers.rs`、`src/database/repositories/result_repo.rs`、`src/database/services/result_service.rs`。
- [ ] 子任务 2 — `[独立]` r2d2 `CustomizeConnection` 设置并上报 PRAGMA 失败。影响：`src/database/pool.rs`。
- [ ] 子任务 3 — `[已确认]` `synchronous=NORMAL` + `wal_autocheckpoint`。影响：`src/database/pool.rs`。
- [ ] 子任务 4 — `[独立]` `EXPLAIN QUERY PLAN` 核实复合索引→删除或改 `(monitor_id,id)` 的迁移；修正失实注释。影响：`migrations/`、`src/database/repositories/result_repo.rs`。
- [ ] 子任务 5 — `[独立]` 评估并调整 `max_size`/`min_idle`。影响：`src/database/pool.rs`。
- [ ] 汇总验证 — `cargo build` + `cargo test`；迁移跑通；分页行为用脚本核对。

## 后果（Consequences）

### 正面
- 控制台翻页代价与页码无关；级联删除可靠（PRAGMA 失败不再静默）；写入 fsync 开销下降。

### 负面 / 代价
- `synchronous=NORMAL` 在断电/OS 崩溃时可能丢失最后若干条未 checkpoint 的记录（库不损坏）——需业务确认。
- 删除/改索引需新增迁移，须保证 up/down 可逆与幂等。

### 风险与跟进
- 索引决策必须以 `EXPLAIN QUERY PLAN` 实测为准，不臆断；若后续新增按时间范围查询，可能重新需要 `(monitor_id, created_at)`。

## 实施结果与偏差（回填）

状态：已实现并通过集成验证（`cargo build` 通过；`cargo test` 161 passed / 0 failed，含 pool/仓储层单测）。

- PRAGMA 建连回调：已完成。实现 `ConnectionPragmas: CustomizeConnection`，在 `on_acquire`（物理建连时一次）设置 `foreign_keys=ON; busy_timeout=5000; synchronous=NORMAL; wal_autocheckpoint=1000`，失败 `error` 记录并作为取连接错误上报（不再 `let _ =` 吞错）；移除 `get_connection` 内每次借连接重跑 PRAGMA 的逻辑。`journal_mode=WAL` 保留。
- `synchronous=NORMAL` + `wal_autocheckpoint=1000`：已启用（用户已确认代价）。
- 连接池：`min_idle(Some(1))`，`max_size(8)` 维持并补依据注释。
- 控制台默认分页：已切为 **keyset 默认**（`result_handlers` 默认调用 `get_check_results_keyset`，响应含 `next_cursor`）；仅在显式需要跳页时走 OFFSET。
- 索引治理：
  - **偏差（基于静态分析，未做活库 EXPLAIN）**：原计划「EXPLAIN 验证后决定保留/改成 `(monitor_id, id)`」。由于当前环境无法对活库跑 `EXPLAIN QUERY PLAN`，改为依据「所有读查询均 `ORDER BY id`，单列索引 `idx_check_result_monitor_id` 隐式含 rowid=id、天然服务 `WHERE monitor_id=? AND id<? ORDER BY id DESC`」这一事实，判定复合索引 `idx_check_result_monitor_created (monitor_id, created_at)` 读侧零使用、仅增最热写表写放大，故**新增迁移将其 DROP**（`migrations/2026-09-26-000000-0000_drop_unused_check_result_index`，down.sql 可逆重建），并修正 `result_repo.rs` 失实注释。EXPLAIN 实测验证留作后续（上线后在真实库核对执行计划）。
