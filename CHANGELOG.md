# Changelog

格式基于 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，版本号遵循 [SemVer](https://semver.org/lang/zh-CN/)。

## [0.4.0] - 2026-09-25

### Added
- 告警历史 `alert_history` 表：告警触发/恢复各写一行（`state`=triggered/recovered），`GET /api/alerts` 游标分页查询 + 控制台告警入口，`alert_state` 只存当前抑制态
- `GET /healthz` 存活探针（免认证，恒 200）
- tag 分组标签：配置可带 `tag`，列表 `?tag=` 筛选、`GET /api/monitors/tags` 去重标签列表、控制台筛选下拉
- HTTP 分段探测可配置 `collect_timings`（默认 true）：关闭时省去每次检查的额外 DNS/TCP/TLS 探测连接
- `TlsConnector` 进程级 OnceLock 缓存（此前每次 HTTP 探测重建 TLS 连接器）
- CSV 日志按日滚动（`monitor_log-YYYY-MM-DD.csv`）+ 常驻写句柄（此前每条记录 open/flush/close，文件无限增长）
- TCP/FTP/DNS 探测超时对齐 `config.timeout`（此前硬编码 5s/3s），附超时生效回归测试
- 启动失败（DB 连接失败/端口绑定失败）退出码改为 1，容器编排可感知（原 exit 0 静默退出）

### Changed
- `SelfDefineMonitorConfig` → `MonitorDefinition`（保留过渡别名，下版本移除）
- RESPONSE_CODE 告警条件字段正名：`contains`→`deny_codes`、`no_contains`→`allowed_codes`（serde alias 兼容旧 JSON，旧配置无需迁移）
- `monitor/types.rs` 并入 `domain/check.rs`（类型定义单一家园）；`UrlLogResult.url` → `monitor`（字段名与存值一致）
- webhook 占位符字面量提为常量 `WEBHOOK_PLACEHOLDER`
- reqwest 显式 `native-tls`：移除 0.13 起默认混入的 rustls(aws-lc-sys) 第二套 TLS 栈（本项目全线 native-tls；aws-lc-sys 的 C 构建也是 CI ubuntu 失败根因）
- 全库 `cargo fmt` 补跑（修复重构后 import 排序漂移）；icmp Linux 分支手写整除改 `div_ceil`（`manual_div_ceil` lint）
- 版本号与 README 同步至 0.4.0

## [0.3.0] - 2026-09-25

### Changed
- 全局类型模块重构：`tools_types.rs` 按关注点拆分为 `domain/` 门面（result 结果 / config 输入配置 / alert 告警通知三个子模块），全库引用同步
- 模块文件名去冗余后缀：12 个监控引擎 `*_monitor.rs` → `monitor/*.rs`，工具 `*_tool.rs` → `tools/*.rs`，`connect_db.rs` → `database/pool.rs`
- 阈值比较符集中为 `CompareOp` 枚举（`parse`/`Display`），消除散落的字符串字面量 match
- 全库 `cargo fmt` 统一格式（import 排序、注释对齐、换行）；新增 `.gitattributes` 固定 LF 换行
- 版本号与 README 同步至 0.3.0

### Fixed
- 补齐 0.2.0 已宣称但仓库中缺失的 GitHub Actions：CI（双平台 test + clippy 0 警告门槛 + fmt 检查）与 Release（`v*` tag 触发双平台二进制 + GHCR 镜像）

## [0.2.0] - 2026-09-17

### Added
- 告警 `CONTENT` 内容校验规则（仅 HTTP）：监控配置的内容规则存在未命中项时告警，与 `AVAILABILITY` 正交
- EMAIL 告警渠道：基于 lettre 的 SMTP 发信（465 隐式 TLS / 587 STARTTLS / 其他端口明文并告警，password 为空跳过认证），后台退避重试
- 钉钉机器人自动加签：配置 `secret` 后自动计算并拼接 `timestamp`/`sign`（URL 已含 timestamp 参数时不覆盖）
- 业务指标提取：`business_metric_fields` 按 JSON 点路径从 HTTP 响应体提取值到 `advanced_available.business_metrics`
- 容器化：Dockerfile（多阶段构建）+ docker-compose.yml + GHCR 镜像发布
- GitHub Actions：CI（双平台 build/test/clippy 0 警告门槛）与 Release（tag 触发二进制 + 镜像）
- 环境变量 `BIND_ADDR` / `LOG_DIR` / `CSV_PATH`：容器部署的监听地址与日志/CSV 落卷

### Changed
- `/api/status` 改为全量读取，不再受分页接口硬编码 1000 条上限截断
- `SMS` 告警渠道从"静默占位（仅打日志）"改为配置期显式拒绝（400）
- 版本号与 README 同步至 0.2.0

### Fixed
- UDP 探测等待超时对齐 `config.timeout`（原硬编码 3 秒）
- 移除未接入主流程的 `fm/` 文件管理死模块
- 修正 `basic_avaliable`→`basic_available`、`advanced_avaliable`→`advanced_available`、`bussiness_metrics`→`business_metrics` 拼写（注意：结果详情 JSON 字段名随之变更；数据库历史记录保留旧键名）

## [0.1.0]

- 首个可用版本：12 种监控类型、三种运行模式、Web API、状态机告警（飞书/钉钉/企微）、Prometheus 指标、会话认证、单文件 Web 控制台
