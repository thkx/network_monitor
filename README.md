# 网络监控器（network_monitor）

基于 Rust 的多类型网络/系统监控器：定时探测目标、结果落库（SQLite）、CSV 日志、规则告警（飞书/钉钉/企业微信 Webhook）与 Web API 管理。

## 功能特性

- **12 种监控类型**：HTTP、ICMP(ping)、TCP、UDP（53端口按DNS协议语义探测并校验应答，其余端口仅检测任意回包）、DNS、FTP（匿名登录握手，验证协议可用而非仅端口开放）、TRACEROUTE(tracert)、CPU、MEMORY、DISK、PROCESS
- **三种运行模式**：
  - `once` — 对 monitor_list.json 中的配置各执行一次探测
  - `monitor --interval N` — 按 N 秒间隔持续探测（无持久化）
  - `server --port N --interval N` — 完整闭环：持久化 + 调度 + 告警 + Web API
    （`--interval` 为未配置 interval 的监控项的默认间隔，秒，缺省 5；调度任务与手动执行共用）
- **Web API**：监控配置 CRUD、分页筛选、启用/禁用（PATCH）、结果查询，配置变更热更新（无需重启）
- **告警体系**：
  - `AVAILABILITY` 通用可用性规则（全部监控类型生效）、`RESPONSE_CODE` 响应码规则（仅 HTTP）
  - 状态机式告警：故障只告警一次，恢复时发送恢复通知，抑制状态持久化到数据库（重启不重复告警）
  - 通知渠道：飞书（可选 secret 签名）、钉钉、企业微信；发送超时 5 秒不阻塞监控任务
- **数据可靠性**：SQLite WAL 模式 + busy_timeout、连接池、迁移自动执行、结果保留策略（默认 30 天）

## 快速开始

```bash
# 1. 配置环境变量（可选，有默认值）
cp .env.example .env

# 2. 运行（首次启动自动执行数据库迁移）
cargo run -- server --port 8080

# 3. 创建一个HTTP监控
curl -X POST http://127.0.0.1:8080/api/monitors ^
  -H "Content-Type: application/json" ^
  -d "{\"target\":\"https://example.com\",\"monitor_type\":\"HTTP\",\"interval\":60}"

# 4. 查询监控列表 / 结果
curl http://127.0.0.1:8080/api/monitors
curl http://127.0.0.1:8080/api/results?monitor_id=1
```

也可以直接在 `monitor_list.json` 中配置监控项，启动时自动导入数据库（按名称去重、幂等）。

## Web API 一览

| 方法   | 路径                                                | 说明                                        |
| ------ | --------------------------------------------------- | ------------------------------------------- |
| GET    | `/api/monitors?page_no=1&page_size=20&enabled=true` | 分页查询监控配置（enabled 可选筛选）        |
| GET    | `/api/monitors/{id}`                                | 查询单个监控配置                            |
| POST   | `/api/monitors`                                     | 创建监控（请求体同 monitor_list.json 单项） |
| PUT    | `/api/monitors/{id}`                                | 更新监控（enabled 不变）                    |
| DELETE | `/api/monitors/{id}`                                | 删除监控（结果记录级联删除）                |
| PATCH  | `/api/monitors/{id}/enabled`                        | 启用/禁用，body: `{"enabled": true}`        |
| GET    | `/api/results?monitor_id=1&page_no=1&page_size=50`  | 分页查询监控结果                            |

## 告警配置示例

```json
{
  "target": "https://example.com",
  "monitor_type": "HTTP",
  "interval": 60,
  "alert_rules": {
    "notify_type": "FEISHU",
    "notify_config": {
      "webhook_url": "https://open.feishu.cn/open-apis/bot/v2/hook/xxx",
      "secret": "飞书开启签名校验时必填，其他情况可省略"
    },
    "rules": [
      { "rule_type": "AVAILABILITY", "condition": {} },
      {
        "rule_type": "RESPONSE_CODE",
        "condition": { "no_contains": [200, 301] }
      }
    ],
    "consecutive_failures": 3,
    "consecutive_successes": 2
  }
}
```

- `notify_type` 支持 `FEISHU` / `DINGTALK` / `WECOM`（大小写不敏感）
- 钉钉机器人若开启"加签"安全设置，需自行在 webhook_url 上拼接 timestamp/sign 参数
- `AVAILABILITY` 对所有监控类型生效；`RESPONSE_CODE` 仅对 HTTP 生效
- **防抖**：`consecutive_failures` 连续 N 次命中才发告警、`consecutive_successes` 连续 M 次正常才发恢复通知（缺省均为 1；可根治网络抖动误报）
- **失败重试**：通知发送失败后自动后台退避重试（1s/5s/30s/2m/5m 共 5 次），不阻塞监控任务循环；重试成功与最终放弃均有明确日志
- **业务码校验**：飞书/钉钉/企微业务失败（签名错、关键词不符等）时 HTTP 仍返回 200，响应体中 `errcode`/`code` 非零同样判定为失败并进入重试，避免告警静默丢失
- **THRESHOLD 阈值规则**（系统资源类）示例：

```json
{
  "target": null,
  "monitor_type": "CPU",
  "alert_rules": {
    "notify_type": "FEISHU",
    "notify_config": { "webhook_url": "https://..." },
    "rules": [
      {
        "rule_type": "THRESHOLD",
        "condition": { "threshold": { "metric": "cpu", "op": ">", "value": 80 } }
      }
    ]
  }
}
```

`metric` 支持 `cpu` / `memory` / `disk`（按总量换算使用率，也可用 `available_bytes`）/ `process`；`op` 支持 `> >= < <= ==`。

## 配置校验规则

创建/更新监控时服务端校验（非法配置返回 400）：

- `interval`：1 ~ 86400 秒
- `timeout`：1 ~ 300000 毫秒
- 内容规则中的 `regex`：必须是合法正则表达式
- `consecutive_failures` / `consecutive_successes`：1 ~ 1000
- THRESHOLD 规则必须配置 `threshold`，且 `op` 必须合法

## 环境变量

| 变量                    | 默认值            | 说明                                   |
| ----------------------- | ----------------- | -------------------------------------- |
| `DATABASE_URL`          | `./db/monitor.db` | SQLite 数据库路径                      |
| `RESULT_RETENTION_DAYS` | `30`              | 监控结果保留天数（每 24 小时清理一次） |
| `RUST_LOG`              | `info`            | 日志级别（trace/debug/info/warn/error，支持按模块覆盖） |
| `ADMIN_USER`            | `admin`           | 登录用户名                             |
| `ADMIN_PASSWORD`        | （空=不启用认证） | 设置后启用登录认证，强烈建议配置       |
| `METRICS_TOKEN`         | （空=metrics开放）| 配置后 `/metrics` 需要 `Bearer` 令牌   |

## 认证

- `ADMIN_PASSWORD` 设置后：除控制台静态页与 `/login` 外的所有路由要求登录
  （HttpOnly 会话 cookie，12 小时有效，进程重启后需重新登录）
- 登录失败按 IP 限流：连续 5 次失败锁定 60 秒（限流检查与失败计数在同一次加锁内原子完成，并发突发不会放大上限；失败记录定期清扫并有容量上限）
- 密码比较使用 SHA-256 摘要常数时间对比（subtle）；凭证仅存于环境变量，不落库
- 会话 cookie 为 HttpOnly + SameSite=Lax；`COOKIE_SECURE=1` 时附加 Secure 标志（HTTPS 部署时开启）
- `/metrics`：配置 `METRICS_TOKEN` 后 Prometheus 需以 `bearer_token` 方式抓取：
  `authorization: Bearer <METRICS_TOKEN>`；未配置时保持开放（导出器惯例，由部署者权衡）

## 日志

基于 `tracing` 的结构化日志，双通道输出：

- **控制台**：人类可读格式（本地时间 + 级别 + 模块）
- **文件**：`logs/network_monitor.log` 按日滚动，无 ANSI 颜色码，适合 grep/采集

级别约定：启动/调度/告警发送成功为 `info`；**检查不可用、配置异常为 `warn`**；每次检查的可
用结果与完整字段（check_id/monitor/response_time_ms/status_code）为 `debug`；持久化、通知发
送失败等需要介入的问题为 `error`。

```bash
RUST_LOG=debug cargo run -- server    # 看到每次检查的完整结构化日志
RUST_LOG="info,hyper=warn" cargo run -- server   # 缺省已静音 hyper/reqwest
```

## 指标端点（Prometheus）

Server 模式暴露 `GET /metrics`（exposition 格式 0.0.4），可直接被 Prometheus 抓取：

```yaml
scrape_configs:
  - job_name: network_monitor
    metrics_path: /metrics
    static_configs:
      - targets: ["127.0.0.1:8080"]
```

指标（标签：`monitor_id` / `name` / `type`）：

| 指标                                   | 类型    | 说明                              |
| -------------------------------------- | ------- | --------------------------------- |
| `network_monitor_up`                   | gauge   | 最近一次检查是否可用（1/0）       |
| `network_monitor_checks_total`         | counter | 检查总次数（`status=ok/failed`）  |
| `network_monitor_last_response_time_milliseconds` | gauge | 最近一次检查耗时       |
| `network_monitor_last_check_timestamp_seconds`    | gauge | 最近一次检查时间       |
| `network_monitor_check_duration_seconds`          | histogram | 检查耗时分布（12个桶：5ms~30s，可用 `histogram_quantile()` 算分位数） |
| `network_monitor_alerting`             | gauge   | 是否处于告警抑制状态（查库实时）  |

计数器为内存态，进程重启后归零（Prometheus `rate()` 兼容 counter 重置）；完整历史以 `check_result` 表为准。

## Web 控制台

Server 模式访问 `http://127.0.0.1:8080/` 即是控制台——单文件原生 HTML/JS，
构建期打包进二进制（`include_str!`），无任何前端构建链与外部资源，离线可用：

- **状态总览**：全部监控的可用/不可用/未检查徽章、告警中标记、耗时与最近检查时间，5 秒自动刷新
- **手动执行**：对任意监控立即执行一次检查（走与定时任务相同的指标/持久化链路），返回完整结果详情
- **历史结果**：按监控查看最近 15 条检查记录
- **配置管理**：JSON 方式新建监控（含告警配置示例模板）、启停、删除

### API 一览

| 方法与路径                     | 说明                                       |
| ------------------------------ | ------------------------------------------ |
| `GET /`                        | Web 控制台页面                             |
| `GET /metrics`                 | Prometheus 指标端点                        |
| `GET /api/status`              | 控制台聚合视图（配置+最新结果+告警状态）   |
| `GET /api/monitors`            | 监控配置分页查询（`enabled` 可选筛选）     |
| `POST /api/monitors`           | 新建监控（非法配置返回 400）               |
| `GET/PUT/DELETE /api/monitors/{id}` | 查询 / 更新 / 删除监控配置            |
| `PATCH /api/monitors/{id}/enabled` | 启停监控（触发调度器热更新）           |
| `POST /api/monitors/{id}/run`  | 手动执行一次检查并持久化结果               |
| `GET /api/results`             | 检查结果分页查询（`monitor_id` 可选过滤）  |

## 存储与写入策略

- **攒批落库**：消费者满 64 条或首个待写结果等待超 1 秒，即以单条多行 INSERT 落库——写锁次数降为批次级，高监控数下避免逐条小事务的锁竞争；批量失败自动降级逐条写入（CSV 日志另有完整备份）
- **WAL 模式**：读写不互斥，配合 busy_timeout 5s 与 foreign_keys 级联删除
- **保留策略**：每 24 小时清理 `RESULT_RETENTION_DAYS` 天前的过期结果
- **优雅关停**：Ctrl+C / SIGTERM 后停止接受请求、停掉全部监控任务并完成最终攒批落库（同步 DB 调用均在阻塞线程池执行，不卡 runtime）

## 开发

```bash
cargo test          # 运行全部测试（49个：纯函数单测 + 临时库集成测试 + 外键级联验证）
cargo clippy --all-targets   # lint（当前0警告）
cargo build         # 构建
```

### 项目结构

```
src/
├── main.rs              # 入口：三种运行模式 + 结果消费/攒批落库等组装逻辑
├── args.rs              # clap 命令行定义（once/monitor/server）
├── logging.rs           # tracing 初始化（控制台 + 按日滚动文件双通道）
├── scheduler.rs         # Server模式调度器（任务句柄管理、热更新重建）
├── async_monitor.rs     # 定时/单次监控任务封装
├── alerts/mod.rs        # 告警引擎（规则评估状态机 + 通知渠道）
├── api/                 # Actix-web 路由与处理器（含配置校验）
├── database/
│   ├── connect_db.rs    # 连接池、WAL、迁移
│   ├── schema.rs        # diesel 表定义
│   ├── models.rs        # 数据模型
│   ├── repositories/    # 仓库层（monitor_config/check_result/alert_state）
│   └── services/        # 业务服务层
├── monitor/             # 12种监控引擎（策略模式 + 工厂）
├── tools/               # HTTP/TLS 探测工具、重试策略
├── tools_types.rs       # 全局类型定义
├── csv_logger.rs        # CSV 日志（check_id 与数据库关联）
└── fm/                  # 文件管理模块（独立功能，暂未接入主流程）
```

### 设计要点

- **告警引擎按任务独占**：每个监控任务持有独立的引擎实例与抑制状态；调度器热更新重建任务时，抑制状态从 `alert_state` 表恢复，避免"故障还在却重复告警"
- **check_id 贯穿**：每次检查生成 u128 ID，CSV 日志以 16 进制输出，同时写入数据库 `metadata_json.check_id`，两边可精确关联
- **失败信息可观测**：HTTP 失败结果携带 `error_kind`（timeout/connect/decode/other）与 `error_message`，告警消息直接附带
