# ADR-0006：安全加固（信息泄漏 / 锁中毒 / 限流 / 凭证 / 口令 / 会话）

- 状态：已接受
- 日期：2026-09-25
- 决策者：项目维护者
- 相关：[ADR-0001](<0001-adopt-adr-and-multi-agent-workflow.md>)（改造簇 E）

## 背景（Context）

API 与鉴权层存在若干安全缺口。部分修复无行为破坏，可直接落地；部分改变对外行为或存储格式，需用户确认后再定稿。

- **错误原文泄漏**：500/400 把 diesel 错误原文（含 SQL/列名/约束名）`format!` 进响应体（`response.rs:39`、`monitor_handlers.rs:34/298/310`、`result_handlers.rs:37`、`status_handlers.rs:56`）。
- **Mutex 中毒级联故障**：中间件 `self.store.lock().expect(...)`（`auth.rs:246`）及多处 handler 用 `.expect` 持锁；任一持锁线程 panic 后，后续所有受保护请求在 `.expect` 处 panic（需重启进程）。
- **限流可绕过 / 可 DoS**：登录限流以 `peer_addr` 为 key（`auth_handlers.rs:32`、`auth.rs:149-170`），反代场景下所有用户共享一个桶（攻击者 5 次失败即可锁定全体合法用户）；`peer_addr` 为 None 时完全不计数。
- **凭证明文存储/回传**：alert_rules 的 SMTP 密码 / webhook secret 明文序列化进 `config_json`（`monitor_handlers.rs:178`），并经 `GET /api/monitors/{id}`（`monitor_handlers.rs:69`）原样回传。
- **口令哈希选型**：管理员口令仅无盐单轮 SHA-256（`auth.rs:60/266`），非 KDF（常数时间比较本身正确）。
- **会话安全**：cookie 默认非 Secure（`auth.rs:55`），CSRF 仅靠 SameSite=Lax 单一防线，无 Origin/Referer 校验。
- **ReDoS 面**：用户正则不限复杂度（`validation.rs:31`）——若引擎侧用 `regex` crate 则本身线性、风险低，需核实。

## 决策（Decision）

分项处理，并明确标注需确认的行为变更：

**无行为破坏，直接落地：**
1. 错误响应：客户端返回通用文案，diesel 详情仅 `tracing::error!` 落日志（配合 ADR 的统一错误信封）。
2. 锁中毒：`lock().unwrap_or_else(|e| e.into_inner())` 从中毒锁恢复，或换 `parking_lot::Mutex`（不中毒）；消除中间件 panic 面。
3. 限流兜底：`peer_addr` 为 None 时保守计入一个全局桶（或拒绝），不再「跳过计数」。

**✅ 用户已确认的行为变更（部署形态：可信反代之后）：**
4. 限流读取 `X-Forwarded-For`：**已确认启用**。在「可信代理」开关开启时解析并校验 XFF 取真实客户端 IP 作为限流 key；开关关闭时回退 `peer_addr`。
5. 凭证脱敏：**已确认**。`GET /api/monitors/{id}`（及列表）对 `notify_config` 的 password/secret 字段返回掩码。加密存储留作后续 ADR。
6. 口令 KDF：**已确认**改用 argon2。`password_digest` 格式与环境变量约定随之调整，需迁移现有口令配置（在 `.env.example` 与 README 注明新格式）。
7. 会话：**已确认**。生产默认 `Secure`（启动检测 BIND_ADDR 非本机却未开 Secure 时 WARN），并增加 Origin/Referer 校验。

**先核实再决定：**
8. ReDoS：确认告警引擎正则是否用 `regex` crate（线性、无灾难回溯）；若是则仅加正则长度上限，风险可控。

## 实施计划（可并行的子任务）

- [ ] 子任务 1 — `[独立]` 错误响应脱敏 + 日志化。影响：`src/api/handles/response.rs` 及各 handler。
- [ ] 子任务 2 — `[独立]` 锁中毒恢复 / parking_lot。影响：`src/auth.rs`、各 handler。
- [ ] 子任务 3 — `[独立]` 限流 None 兜底。影响：`src/auth.rs`、`src/api/handles/auth_handlers.rs`。
- [ ] 子任务 4 — `[已确认]` XFF 可信代理解析。影响：`src/api/handles/auth_handlers.rs`、`src/auth.rs`。
- [ ] 子任务 5 — `[已确认]` GET 凭证脱敏。影响：`src/api/handles/monitor_handlers.rs`。
- [ ] 子任务 6 — `[已确认]` 口令 KDF（argon2）+ 迁移说明。影响：`src/auth.rs`、`Cargo.toml`、`.env.example`。
- [ ] 子任务 7 — `[已确认]` cookie Secure 默认 + Origin 校验。影响：`src/auth.rs`。
- [ ] 子任务 8 — `[独立]` 核实正则引擎 + 加长度上限。影响：`src/api/handles/validation.rs`、`src/alerts/rules.rs`。
- [ ] 汇总验证 — `cargo build` + `cargo test`；手动核对鉴权/限流/脱敏行为。

## 后果（Consequences）

### 正面
- 消除信息泄漏、单 panic 级联、限流绕过/DoS；凭证不再经 API 明文回传；口令与会话符合最佳实践。

### 负面 / 代价
- 子任务 4–7 改变对外行为/存储格式，需迁移与部署沟通（故列为需确认）。

### 风险与跟进
- 凭证**加密存储**（而非仅脱敏回传）未在本 ADR 范围，留作后续 ADR。
- XFF 解析若信任边界设置不当会引入新的伪造风险，必须限定「可信代理」开关 + 校验链。

## 实施结果与偏差（回填）

状态：已实现并通过集成验证（`cargo build` 通过；`cargo test` 161 passed / 0 failed）。

- 子任务 1（错误脱敏）：已完成。`response.rs::internal_error` 及相关 handler 对客户端返回通用文案，详细错误仅 `tracing::error!` 落日志。
- 子任务 2（锁中毒恢复）：已完成。中间件与各 handler 的 `.lock().expect(...)` 统一改为 `lock().unwrap_or_else(|e| e.into_inner())`，消除级联 panic（未引入 parking_lot）。
- 子任务 3（限流 None 兜底）：已完成。拿不到 IP 时计入固定兜底桶 `RATE_LIMIT_FALLBACK_IP`，不跳过计数。
- 子任务 4（XFF 可信代理，已确认启用）：已完成。新增 `TRUST_PROXY`（默认 false）开关；开启时 `client_ip` 取 `X-Forwarded-For` 最左侧并做 IP 解析校验，否则回退 `peer_addr`。
- 子任务 5（GET 凭证脱敏）：已完成，读接口对 notify_config 中 password/secret 字段掩码，落库逻辑不变。
- 子任务 6（argon2，已确认）：已完成。口令 KDF 升级为 **argon2id**；支持 `ADMIN_PASSWORD_HASH`（PHC 字符串，推荐）或 `ADMIN_PASSWORD`（明文，启动时派生）。
  - **实现细节**：随机盐取自 `uuid v4` 的 16 字节（复用既有 uuid 依赖，未引入 rand/getrandom）；`verify_password` 用 argon2 的常数时间校验；用户名仍走 `sha256 + ct_eq` 常数时间比较。`.env.example` 已补两种配置方式与生成指引。
  - **代价**：登录校验显著变慢（argon2 刻意如此），对分布式多 IP flood 有 CPU 放大（见 ADR-0004 回填）。
- 子任务 7（cookie Secure，已确认）：已完成。`COOKIE_SECURE` 缺省按 `BIND_ADDR` 推断（非本机监听默认开启 Secure，本机默认关闭），可显式覆盖。
- 子任务 8（ReDoS）：已完成。核实告警/校验正则均为 `regex` crate（线性时间、无灾难回溯，ReDoS 风险极低）；`validation.rs` 增加正则长度上限校验作为纵深防御。
- **集成期修复（编排者）**：Agent 新增的 `is_local_bind` 对裸 IPv6 `::1` 判定错误（简单按最后一个冒号切端口会把 `::1` 切成 `:`），已修正为「中括号 IPv6 / 恰好一个冒号才切端口 / 其余整体为 host」，其配套单测通过。另把 `sweep_caps_tracked_ips_under_flood` 单测由「跑 1 万次真实 argon2 登录」改为「直接构造 attempts 条目」，仅验证 sweep 容量驱逐分支，避免 argon2 使该用例耗时数分钟。
