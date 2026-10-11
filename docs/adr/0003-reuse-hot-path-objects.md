# ADR-0003：消除热路径上的重复初始化（复用重量级对象）

- 状态：已接受
- 日期：2026-09-25
- 决策者：项目维护者
- 相关：[ADR-0001](<0001-adopt-adr-and-multi-agent-workflow.md>)（改造簇 B）

## 背景（Context）

多个每轮 check 都会执行的热路径上，重量级对象被反复重建或重复编译：

- `DnsMonitor` 每轮 `TokioAsyncResolver::tokio_from_system_conf()`（`monitor/dns.rs:32`）：重做系统解析配置 IO、丢弃内置 DNS 缓存，并污染「解析耗时」统计。
- 内容校验正则每轮每条规则 `Regex::new`（`monitor/http.rs:197`）、响应码规则正则每次评估 `Regex::new`（`alerts/rules.rs:69`）：对固定规则集的纯重复编译。
- `DiskMonitor` 每轮 `Disks::new_with_refreshed_list()`（`monitor/disk.rs:19`）：与 cpu/memory/process 复用常驻 `System` 的策略不一致，每轮全量枚举挂载点。
- SMTP transport 每封邮件、每次重试都重建（`tools/smtp.rs:52-77`），承载 TLS 栈初始化却不复用，且无显式超时。

HttpMonitor 已持有 `client` 字段复用（`monitor/http.rs`），是本 ADR 要推广的既有正确范式。

## 候选方案（Options）

1. **各结构持有预构建对象 / 配置加载期预编译（本决策）** — 优点：范式与 HttpMonitor 一致、作用域清晰；缺点：trait `check(&self)` 需内部可变性（`OnceCell`/`Mutex`）。
2. **全局 `lazy_static`/`OnceLock` 单例** — 优点：实现省事；缺点：跨监控共享状态、生命周期与配置热更新耦合不清晰。
3. **维持现状** — 放弃可观的热路径收益。

## 决策（Decision）

采用方案 1：

- `DnsMonitor` 持有 resolver，构造期或 `tokio::sync::OnceCell` 懒初始化一次复用。
- 正则在配置加载 / `HttpMonitorConfig`、规则结构构建时预编译并缓存（如 `Vec<Regex>` / 结构持 `Regex`）；非法正则在加载期一次性报错（API 层已预校验，正好在该层编译）。
- `DiskMonitor` 持 `Mutex<Disks>`，check 内 `refresh_list()`/`refresh()` 增量刷新。
- SMTP transport 按配置缓存复用，并设置显式连接/操作超时。

## 实施计划（可并行的子任务）

- [ ] 子任务 1 — `[独立]` DNS resolver 复用。影响：`src/monitor/dns.rs`。
- [ ] 子任务 2 — `[独立]` 正则预编译缓存：内容校验（`src/monitor/http.rs`）+ 响应码规则（`src/alerts/rules.rs`）。
- [ ] 子任务 3 — `[独立]` DiskMonitor 持 `Mutex<Disks>` 增量刷新。影响：`src/monitor/disk.rs`。
- [ ] 子任务 4 — `[独立]` SMTP transport 复用 + 显式超时。影响：`src/tools/smtp.rs`（超时上限与 ADR-0004 协同）。
- [ ] 汇总验证 — `cargo build` + `cargo test`（含既有协议/规则单测）。

## 后果（Consequences）

### 正面
- DNS/HTTP/磁盘/邮件热路径减少重复初始化与分配，统计口径更干净（解析耗时不含初始化）。

### 负面 / 代价
- `check(&self)` 引入内部可变性，需保证不在 await 点跨持 `std::Mutex`（参考 cpu.rs 的处理）。

### 风险与跟进
- resolver 复用后其内置缓存可能让「解析耗时」偏乐观——这是期望行为（反映真实缓存命中），如需每轮强制冷查询另议。

## 实施结果与偏差（回填）

状态：已实现并通过集成验证（`cargo build` 通过；`cargo test` 161 passed / 0 failed）。

- 子任务 1（DNS）：已完成。`DnsMonitor` 持 `OnceCell<Arc<TokioAsyncResolver>>` 懒初始化复用；初始化失败返回失败结果 + `warn`，不缓存失败态（下轮可重试），不再 panic。
- 子任务 2（正则缓存）：已完成，两处策略不同——
  - 响应码规则（`alerts/rules.rs` + `engine.rs`）：**构造期一次性预编译**到 `AlertsEngine.response_code_regexes: Vec<Option<Regex>>`，`evaluate` 按索引取用。
  - **偏差**：HTTP 内容校验（`monitor/http.rs`）无法在 `HttpMonitor::new()` 预编译（工厂构造时拿不到 config/rules），改为**按模式串懒编译 + 进程内 `Mutex<HashMap<String, Option<Regex>>>` 缓存**；实例在 interval 循环中跨轮复用，稳态下不再重复编译，同样把编译移出热路径。非法正则记一次 `warn` 并缓存 `None`（语义等同「未匹配」）。
- 子任务 3（Disk）：已完成。`DiskMonitor` 持 `Mutex<Disks>`，`check` 内 `refresh_list()/refresh()` 增量刷新，锁在无 await 的作用域内释放。
- 子任务 4（SMTP）：已完成。按 `(host,port,username,password)` 键缓存 `AsyncSmtpTransport` 复用（`OnceLock<Mutex<HashMap<...>>>`），显式 `.timeout(10s)`；原「非 loopback 明文凭证拦截」原样保留。
- 附带（原 ADR 未列）：`HttpMonitor` 的 reqwest `Client` 由 `expect` 改为 `Option<Client>`，构建失败记 `error` 并在 check 返回失败结果，不再 panic。
