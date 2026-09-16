# 网络监控器（network_monitor）

基于 Rust 的多类型网络/系统监控器：定时探测目标、结果落库（SQLite）、CSV 日志、规则告警（飞书/钉钉/企业微信 Webhook）与 Web API 管理。

## 功能特性

- **12 种监控类型**：HTTP、ICMP(ping)、TCP、UDP、DNS、FTP、TRACEROUTE(tracert)、CPU、MEMORY、DISK、PROCESS
- **三种运行模式**：
  - `once` — 对 monitor_list.json 中的配置各执行一次探测
  - `monitor --interval N` — 按 N 秒间隔持续探测（无持久化）
  - `server --port N` — 完整闭环：持久化 + 调度 + 告警 + Web API
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

## 开发

```bash
cargo test          # 运行全部测试（49个：纯函数单测 + 临时库集成测试 + 外键级联验证）
cargo clippy --all-targets   # lint（当前0警告）
cargo build         # 构建
```

### 项目结构

```
src/
├── main.rs              # 入口：三种运行模式 + build_monitor_config 等核心组装逻辑
├── args.rs              # clap 命令行定义（once/monitor/server）
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
