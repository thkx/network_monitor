# ADR-0004：统一超时与资源预算（消除慢路径失控与重试风暴）

- 状态：已接受
- 日期：2026-09-25
- 决策者：项目维护者
- 相关：[ADR-0001](<0001-adopt-adr-and-multi-agent-workflow.md>)（改造簇 C）

## 背景（Context）

多处缺乏「整体时间上界」或「资源数量上界」，使慢路径可远超配置预期，或在大面积故障时积累资源：

- `get_dns_tcp_tls_performance`（`tools/http.rs:66-134`）对解析出的**每个** IP 串行做 TCP 3s + TLS 3s 超时，无总预算：N 个地址最坏 ≈ N×6s，远超监控配置 `timeout`。
- `run_monitor_once` 的 `rx.recv().await`（`monitor_handlers.rs:308`）无超时，底层探测挂起则 handler 永久占用一个 worker。
- 定时 `interval`（`async_monitor.rs:48`）用默认 `MissedTickBehavior::Burst`，慢探测（icmp/traceroute）超过 interval 后会「背靠背」连打，失去节流。
- HTTP 响应体（`monitor/http.rs:408`）、webhook 响应体（`alerts/notify.rs:261`）均 `text().await` 无大小上限，异常/恶意超大 body 有 OOM 风险。
- ftp 握手（`monitor/ftp.rs:41-83`）只约束单步，最坏 ≈ 4×`timeout`，与 tcp/udp/icmp 的「整体上界」语义不一致。
- 发送失败后无上限 `tokio::spawn` 后台重试（`notify.rs:91/212`），退避 `[1,5,30,120,300]` 单任务最长存活 ≈456s，大面积故障时形成重试风暴。

## 候选方案（Options）

1. **引入统一的超时/预算约定并逐处落地（本决策）** — 优点：语义一致、可预测；缺点：需在多处小心传递 deadline。
2. **仅加文档说明不改代码** — 不解决失控问题。

## 决策（Decision）

采用方案 1，统一约定：

- **整体 deadline 优先于单步超时**：`get_dns_tcp_tls_performance` 接收总预算（取 `config.timeout`），命中首个可连地址即停止，并限制尝试地址数；ftp 握手用外层 `tokio::time::timeout(total)` 包裹，或传递递减的剩余预算。
- **once 探测加超时**：`run_monitor_once` 用 `tokio::time::timeout` 包裹 `recv`，超时返回 504。
- **节流语义**：`interval` 设 `MissedTickBehavior::Skip`。
- **响应体限长**：HTTP / webhook 响应仅读取前若干 KB（可配置上限，默认如 64–256KB）用于校验/业务码解析。
- **重试资源上界**：后台重试任务用全局 `Semaphore` 限并发，超额时丢弃并计数（配合 metrics 暴露）。

## 实施计划（可并行的子任务）

- [ ] 子任务 1 — `[独立]` `get_dns_tcp_tls_performance` 整体 deadline + 首个可连即停。影响：`src/tools/http.rs`。
- [ ] 子任务 2 — `[独立]` `run_monitor_once` 超时→504。影响：`src/api/handles/monitor_handlers.rs`。
- [ ] 子任务 3 — `[独立]` `interval` 改 `Skip`。影响：`src/async_monitor.rs`。
- [ ] 子任务 4 — `[独立]` HTTP body 限长读取。影响：`src/monitor/http.rs`。
- [ ] 子任务 5 — `[独立]` webhook body 限长读取。影响：`src/alerts/notify.rs`。
- [ ] 子任务 6 — `[独立]` ftp 握手整体超时。影响：`src/monitor/ftp.rs`。
- [ ] 子任务 7 — `[独立]` 后台重试 `Semaphore` 限并发 + 溢出计数。影响：`src/alerts/notify.rs`（与 ADR-0006 的 EMAIL 语义协同）。
- [ ] 汇总验证 — `cargo build` + `cargo test`；核对慢目标/多 IP/超大 body 场景不再超预算。

## 后果（Consequences）

### 正面
- 慢路径耗时可预测、不再超配置上界；杜绝 body OOM 与重试风暴。

### 负面 / 代价
- body 限长意味着对超大响应只校验前缀，需确认内容/业务码字段位于前缀内（通常成立）。
- `Skip` 行为下，单轮超时的监控会跳过本应触发的 tick（这是期望的节流语义）。

### 风险与跟进
- body 上限取值需按真实目标调参；留作可配置项，默认保守。

## 实施结果与偏差（回填）

状态：已实现并通过集成验证（`cargo build` 通过；`cargo test` 161 passed / 0 failed）。

- 子任务 1（dns/tcp/tls 整体预算）：已完成。`tools::http::get_dns_tcp_tls_performance` **新增 `budget: Duration` 参数**（公共签名变化，调用方 `monitor/http.rs` 已同步传入 `config.timeout`）：各步超时取「剩余预算 vs 固定 3s」较小者，命中首个可连地址即停，最多尝试 3 个地址。
- 子任务 2（`run_monitor_once` 超时→504）：已完成，`rx.recv()` 用 `tokio::time::timeout` 包裹，超时返回 504。
- 子任务 3（interval `Skip`）：已完成（`async_monitor.rs`）。
- 子任务 4（HTTP body 限长）：已完成，上限 256KB，`Response::chunk()` 累积截断。
  - **偏差**：原按 Content-Type charset 解码的 `text()` 改为 **UTF-8 lossy** 解码（受文件归属与「不新增 encoding 依赖」约束）。JSON/json-path 与 UTF-8 正文不受影响；仅非 UTF-8 charset（如 GBK）正文的内容匹配由「charset 解码」降级为「lossy」；`res_charset` 响应头解析不受影响。
- 子任务 5（webhook body 限长）：已完成，上限 64KB。
- 子任务 6（ftp 握手整体超时）：已完成，单层 `timeout(total_deadline, …)` 包裹整个握手。
- 子任务 7（后台重试并发封顶）：已完成。进程级 `Semaphore`（许可 64）限制 webhook/EMAIL 后台重试任务数，`permit` 持有至任务结束，拿不到许可时记 `warn` + `AtomicU64` 计数后放弃本次后台重试。
  - **偏差（与 ADR-0006 协同，已确认接受）**：EMAIL 路径在许可耗尽时返回 `Deferred` 而非原来的乐观 `Delivered`——避免「整单从未发起却置位抑制状态」导致静默丢告警；状态机在下一轮重发。
- 附带风险（argon2 关联）：登录校验改 argon2 后，针对**大量不同源 IP**的登录 flood 会放大服务端 CPU（每个新 IP 一次 argon2）。按 IP 的锁定与 `MAX_TRACKED_IPS` 容量上限可约束单 IP 与内存，但分布式 flood 的 CPU 放大为已知可接受代价，必要时另起 ADR 评估全局限流/验证码。
