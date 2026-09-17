# Changelog

格式基于 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，版本号遵循 [SemVer](https://semver.org/lang/zh-CN/)。

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
