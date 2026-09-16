// 日志模块
mod csv_logger;
use csv_logger::{CsvLogger, UrlLogResult};

// 定时监控模块
mod async_monitor;
use async_monitor::{AsyncMonitor, MonitorResultMessage, ResultRoute};

// 告警引擎模块（规则判断 + 通知发送）
mod alerts;
use alerts::AlertsEngine;

// Web API 模块
mod api;

// 调度器模块（Server模式：统一持有监控任务句柄，支持配置热更新）
mod scheduler;
use scheduler::Scheduler;

// 数据库持久化模块
mod database;
use database::connect_db::establish_database_connection;
use database::models::CheckResultModelInsert;
use database::repositories::monitor_repo::MonitorRepository;
use database::repositories::result_repo::CheckResultRepository;
use database::services::build_monitor_insert;
use database::services::monitor_service::MonitorService;
use database::services::result_service::ResultService;

mod args;
use args::{Args, Commands};

// 监控引擎模块（基于策略模式+工厂模式的不同类型监控系统）
mod monitor;
use monitor::MonitorFactory;
use monitor::types::{
    CheckResult, CheckResultDetail, CpuMonitorConfig, DiskMonitorConfig, DnsMonitorConfig,
    FtpMonitorConfig, HttpMonitorConfig, IcmpMonitorConfig, MemoryMonitorConfig, MonitorConfig,
    MonitorConfigDetail, ProcessMonitorConfig, TcpMonitorConfig, TracerouteMonitorConfig,
    UdpMonitorConfig, UnknownQueryConfig,
};

// 工具模块
mod tools;

// 全局通用的类型定义模块
mod tools_types;
use tools_types::{
    HttpBody, HttpBodyConfig, HttpMethodTypes, MonitorType, SelfDefineMonitorConfig,
};

// 文件管理模块（独立功能，暂未接入主流程）
// #[allow(dead_code)]
// mod fm;

use actix_web::{App, HttpServer, web};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use clap::Parser;
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

#[actix_web::main]
async fn main() {
    // 读取 .env（如 DATABASE_URL）；没有 .env 或读取失败时静默跳过，使用真实环境变量或代码内默认值
    dotenvy::dotenv().ok();
    println!("网络监控器 启动...");
    // 初始化日志结构体
    let logger = CsvLogger::new("monitor_log.csv");
    // 获取待监控的列表（JSON配置文件，每个监控引擎的参数都是一个对象，方便后续通过页面来配置）
    let monitor_list = read_monitor_list("monitor_list.json");
    // 获取命令行参数
    let args = Args::parse();
    // 匹配命令行参数
    match args.command {
        Commands::Once => {
            if monitor_list.is_empty() {
                println!("监控列表为空，请在 monitor_list.json 中添加监控配置。");
                return;
            }
            // 对每个监控配置执行一次监控
            for entry in &monitor_list {
                let name = display_name(entry);
                let config = build_monitor_config(entry, 5);
                let monitor = MonitorFactory::create_monitor(config.monitor_type);
                let mut rx = AsyncMonitor::create_once_monitoring(monitor, config).await;
                if let Some(result) = rx.recv().await {
                    log_result(&logger, &name, &result);
                    // 命中告警规则时通过告警引擎发送通知
                    if let Some(alert_rules) = entry.alert_rules.as_ref() {
                        let mut engine = AlertsEngine::new(alert_rules.clone());
                        if let Err(e) = engine.check(&result).await {
                            eprintln!("告警检查失败: {}", e);
                        }
                    }
                }
            }
        }
        Commands::Monitor { interval } => {
            if monitor_list.is_empty() {
                println!("监控列表为空，请在 monitor_list.json 中添加监控配置。");
                return;
            }
            // 所有监控任务共享一个通道，结果由单一消费者顺序处理，避免多通道轮询消费互相阻塞
            let (tx, mut rx) = mpsc::channel::<MonitorResultMessage>(100);
            for entry in &monitor_list {
                let name = display_name(entry);
                let config = build_monitor_config(entry, interval);
                // 通过工厂创建对应类型的监控引擎，未配置interval的项使用命令行传入的默认间隔
                let monitor = MonitorFactory::create_monitor(config.monitor_type);
                // 告警引擎下沉到任务内部：每个任务独占引擎与抑制状态
                let engine = entry.alert_rules.clone().map(AlertsEngine::new);
                AsyncMonitor::create_interval_monitoring(
                    monitor,
                    config,
                    ResultRoute {
                        name,
                        monitor_id: None,
                    },
                    engine,
                    tx.clone(),
                );
            }
            drop(tx); // 丢弃主发送端，各监控任务各自持有clone
            // 单一消费者：写日志（告警检查已在各监控任务内部完成）
            while let Some(message) = rx.recv().await {
                log_result(&logger, &message.route.name, &message.result);
            }
        }
        Commands::Server { port } => {
            // Server模式：数据持久化 + 调度监控 + 告警 + Web API 的功能闭环
            run_server(port, monitor_list).await;
        }
    }
}

// Server命令入口：
// 1.建立数据库连接 2.导入monitor_list.json 3.从数据库加载配置并调度监控
// 4.后台消费监控结果（CSV日志+数据库持久化+告警） 5.启动Web API服务
async fn run_server(port: u16, monitor_list: Vec<SelfDefineMonitorConfig>) {
    // 1. 建立数据库连接（带重试，自动执行迁移建表）
    let pool = match establish_database_connection().await {
        Ok(pool) => pool,
        Err(e) => {
            eprintln!("数据库连接失败: {}", e);
            return;
        }
    };
    let monitor_service = MonitorService::new(MonitorRepository::new(pool.clone()));
    let result_service = ResultService::new(CheckResultRepository::new(pool.clone()));

    // 2. 将 monitor_list.json 中的配置导入数据库（按名称去重，幂等）
    import_monitor_list(&monitor_service, &monitor_list);

    // 2.5 数据保留策略：启动时清理一次过期监控结果，此后每24小时重复
    // 保留天数通过环境变量 RESULT_RETENTION_DAYS 配置（.env），默认30天
    let retention_days = std::env::var("RESULT_RETENTION_DAYS")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(30);
    let result_service_for_cleanup = result_service.clone();
    tokio::spawn(async move {
        loop {
            match result_service_for_cleanup.delete_expired(retention_days) {
                Ok(n) if n > 0 => println!("已清理 {} 天前的监控结果: {} 条", retention_days, n),
                Ok(_) => {}
                Err(e) => eprintln!("清理过期监控结果失败: {}", e),
            }
            tokio::time::sleep(std::time::Duration::from_secs(24 * 60 * 60)).await;
        }
    });

    // 3. 后台任务持续消费监控结果：写CSV日志 + 持久化到数据库（告警检查已下沉到各监控任务）
    let (tx, rx) = mpsc::channel::<MonitorResultMessage>(100);
    let result_service_for_loop = result_service.clone();
    tokio::spawn(async move {
        consume_results(result_service_for_loop, rx).await;
    });

    // 4. 调度器加载启用配置并启动定时监控；API 增删改配置后可整体重建实现热更新
    let mut scheduler = Scheduler::new(pool.clone());
    scheduler.set_sender(tx);
    scheduler.reload_all(&monitor_service);
    let scheduler = web::Data::new(Arc::new(Mutex::new(scheduler)));

    // 5. 启动Web API服务
    match HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(monitor_service.clone()))
            .app_data(web::Data::new(result_service.clone()))
            .app_data(scheduler.clone())
            .configure(api::configure_routes)
    })
    .bind(("127.0.0.1", port))
    {
        Ok(server) => {
            println!("Web API 已启动: http://127.0.0.1:{}/api/monitors", port);
            if let Err(e) = server.run().await {
                eprintln!("Web API 服务异常退出: {}", e);
            }
        }
        Err(e) => eprintln!("Web API 端口 {} 绑定失败: {}", port, e),
    }
}

// 持续接收各监控任务的结果：写CSV、持久化数据库
// 共享通道 + 单一消费者：所有监控任务的结果在此顺序处理，互不阻塞
// 告警检查已下沉到各监控任务内部，由任务独占引擎与抑制状态
async fn consume_results(
    result_service: ResultService,
    mut rx: mpsc::Receiver<MonitorResultMessage>,
) {
    let logger = CsvLogger::new("monitor_log.csv");
    while let Some(message) = rx.recv().await {
        log_result(&logger, &message.route.name, &message.result);
        // 结果持久化到check_result表
        if let Some(monitor_id) = message.route.monitor_id {
            persist_result(&result_service, monitor_id, &message.result);
        }
    }
}

// 把监控结果持久化到check_result表
// metadata_json = {"check_id": "<hex>", "details": {...}}：检查ID一并入库，
// CSV日志里的check_id即可关联到这条记录（此前两者无法对应）
fn persist_result(result_service: &ResultService, monitor_id: i32, result: &CheckResult) {
    let (status, response_time, _status_code) = log_fields(result);
    let mut metadata = serde_json::Map::new();
    metadata.insert(
        "check_id".to_string(),
        serde_json::Value::String(format!("{:x}", result.id)),
    );
    if let Ok(details) = serde_json::to_value(&result.details) {
        metadata.insert("details".to_string(), details);
    }
    let metadata_json = serde_json::to_string(&serde_json::Value::Object(metadata)).ok();
    if let Err(e) = result_service.save_check_result(&CheckResultModelInsert {
        monitor_id,
        monitor_type: result.monitor_type.to_string(),
        status: if status { 1 } else { 0 },
        response_time: response_time.min(i32::MAX as u128) as i32,
        metadata_json,
    }) {
        eprintln!("监控结果持久化失败 (monitor_id={}): {}", monitor_id, e);
    }
}

// 把JSON配置导入monitor_config表（name有唯一约束，按名称去重保证幂等）
fn import_monitor_list(monitor_service: &MonitorService, monitor_list: &[SelfDefineMonitorConfig]) {
    for entry in monitor_list {
        let name = display_name(entry);
        match monitor_service.find_by_name(&name) {
            Ok(Some(_)) => continue, // 已存在，跳过
            Ok(None) => {
                let insert = build_monitor_insert(entry);
                match monitor_service.create_monitor(&insert) {
                    Ok(_) => println!("监控配置已导入: {}", name),
                    Err(e) => eprintln!("监控配置导入失败 {}: {}", name, e),
                }
            }
            Err(e) => eprintln!("查询监控配置 {} 失败: {}", name, e),
        }
    }
}

// 监控项的展示名称：优先使用target，没有target时（如CPU/MEMORY监控）使用监控类型名
fn display_name(entry: &SelfDefineMonitorConfig) -> String {
    entry
        .target
        .clone()
        .unwrap_or_else(|| entry.monitor_type.to_string())
}

// 根据JSON配置项构建监控任务配置
// default_interval：配置项未指定interval时使用的默认监控间隔（来自命令行参数）
fn build_monitor_config(entry: &SelfDefineMonitorConfig, default_interval: u64) -> MonitorConfig {
    let monitor_type = entry.monitor_type;
    // 按监控类型构建各自的详情参数
    let details = match monitor_type {
        MonitorType::Http => {
            // HTTP请求方法，未配置时默认GET
            let method = entry.method.clone().unwrap_or(HttpMethodTypes::Get);
            let mut url = entry.target.clone().unwrap_or_default();
            // GET/HEAD类请求的params追加为URL查询参数（form_urlencoded做URL编码，值含&/=/空格/中文等特殊字符也安全）
            if !entry.params.is_empty()
                && matches!(method, HttpMethodTypes::Get | HttpMethodTypes::Head)
            {
                let pairs: Vec<(String, String)> = entry
                    .params
                    .iter()
                    .map(|(k, v)| {
                        // 字符串值取原文字面量，其他JSON类型用其JSON文本表示
                        let val = match v {
                            serde_json::Value::String(s) => s.clone(),
                            other => other.to_string(),
                        };
                        (k.clone(), val)
                    })
                    .collect();
                let query = url::form_urlencoded::Serializer::new(String::new())
                    .extend_pairs(pairs)
                    .finish();
                let sep = if url.contains('?') { "&" } else { "?" };
                url = format!("{}{}{}", url, sep, query);
            }
            // headers配置转换为HeaderMap，非法的键值直接忽略
            let mut header_map = HeaderMap::new();
            for (k, v) in &entry.headers {
                if let (Ok(name), Ok(value)) = (
                    reqwest::header::HeaderName::from_bytes(k.as_bytes()),
                    HeaderValue::from_str(v),
                ) {
                    header_map.insert(name, value);
                }
            }
            // 用户已显式配置Content-Type时不再补充默认值
            let has_content_type = entry
                .headers
                .keys()
                .any(|k| k.eq_ignore_ascii_case("content-type"));
            // 请求体构造：显式body配置优先；未配置body时保留旧的params语义（POST类请求params作为JSON请求体）
            let body = if let Some(body_cfg) = entry.body.as_ref() {
                match body_cfg {
                    HttpBodyConfig::Text { content } => {
                        if !has_content_type {
                            header_map.insert(CONTENT_TYPE, HeaderValue::from_static("text/plain"));
                        }
                        Some(HttpBody::Text(content.clone()))
                    }
                    HttpBodyConfig::Json { content } => Some(HttpBody::Json(content.clone())),
                    HttpBodyConfig::Binary { content } => {
                        // binary的content为base64编码，解码失败时忽略该body并告警
                        match BASE64_STANDARD.decode(content.as_bytes()) {
                            Ok(bytes) => {
                                if !has_content_type {
                                    header_map.insert(
                                        CONTENT_TYPE,
                                        HeaderValue::from_static("application/octet-stream"),
                                    );
                                }
                                Some(HttpBody::Binary(bytes))
                            }
                            Err(e) => {
                                eprintln!("body(binary) base64解码失败，本次请求不携带body: {}", e);
                                None
                            }
                        }
                    }
                    HttpBodyConfig::Empty => Some(HttpBody::Empty),
                }
            } else if !entry.params.is_empty()
                && matches!(
                    method,
                    HttpMethodTypes::Post | HttpMethodTypes::Put | HttpMethodTypes::Patch
                )
            {
                Some(HttpBody::Json(serde_json::Value::Object(
                    entry.params.clone(),
                )))
            } else {
                None
            };
            let headers = if header_map.is_empty() {
                None
            } else {
                Some(header_map)
            };
            MonitorConfigDetail::Http(HttpMonitorConfig {
                url,
                method,
                timeout: entry.timeout.unwrap_or(5000),
                headers,
                body,
                rules: entry.content_evaluation_rules.clone(),
            })
        }
        MonitorType::Icmp => MonitorConfigDetail::Icmp(IcmpMonitorConfig {}),
        MonitorType::Tcp => MonitorConfigDetail::Tcp(TcpMonitorConfig {}),
        MonitorType::Udp => MonitorConfigDetail::Udp(UdpMonitorConfig {}),
        MonitorType::Dns => MonitorConfigDetail::Dns(DnsMonitorConfig {}),
        MonitorType::Ftp => MonitorConfigDetail::Ftp(FtpMonitorConfig {}),
        MonitorType::Traceroute => MonitorConfigDetail::Traceroute(TracerouteMonitorConfig {}),
        MonitorType::Cpu => MonitorConfigDetail::Cpu(CpuMonitorConfig {}),
        MonitorType::Memory => MonitorConfigDetail::Memory(MemoryMonitorConfig {}),
        MonitorType::Disk => MonitorConfigDetail::Disk(DiskMonitorConfig {}),
        MonitorType::Process => MonitorConfigDetail::Process(ProcessMonitorConfig {}),
        MonitorType::Unknown => MonitorConfigDetail::Unknown(UnknownQueryConfig {
            description: "未知监控类型".to_string(),
        }),
    };
    MonitorConfig {
        target: entry.target.clone(),
        interval: Some(entry.interval.unwrap_or(default_interval)),
        monitor_type,
        details,
    }
}

// 将监控结果写入CSV日志并打印到控制台
fn log_result(logger: &CsvLogger, name: &str, result: &CheckResult) {
    let (status, response_time, status_code) = log_fields(result);
    // 兜底类型的诊断信息：把Unknown的description暴露出来，说明为什么走到了兜底
    if let CheckResultDetail::Unknown(u) = &result.details {
        eprintln!("警告: {}", u.description);
    }
    // 检查ID以16进制输出，用于跨CSV日志与数据库关联同一次检查
    let check_id = format!("{:x}", result.id);
    logger
        .log(UrlLogResult {
            check_id,
            url: name.to_string(),
            status,
            response_time,
            status_code,
        })
        .unwrap();
    println!(
        "[{:x}] {} 状态：{}，响应时间：{}ms，状态码：{:?}",
        result.id, name, status, response_time, status_code
    );
}

// 把不同监控类型的结果统一转换为日志记录所需的字段 (状态, 耗时ms, 状态码)
fn log_fields(result: &CheckResult) -> (bool, u128, Option<u16>) {
    match &result.details {
        CheckResultDetail::Http(r) => (
            r.basic_avaliable.is_reachable,
            r.performance_timings.total_time,
            r.basic_avaliable.res_status_code,
        ),
        CheckResultDetail::Icmp(r) => (r.is_alive, r.elapsed_ms, None),
        CheckResultDetail::Tcp(r) => (r.connected, r.elapsed_ms, None),
        CheckResultDetail::Udp(r) => (r.response_received, r.elapsed_ms, None),
        CheckResultDetail::Dns(r) => (r.resolved, r.elapsed_ms, None),
        CheckResultDetail::Ftp(r) => (r.connected, r.elapsed_ms, None),
        CheckResultDetail::Traceroute(r) => (r.success, 0, None),
        // 系统资源类监控（CPU/MEMORY/DISK/PROCESS等）能执行即视为成功
        _ => (result.status, 0, None),
    }
}

// 获取监控的列表（从JSON配置文件读取，每个监控引擎的参数都是一个对象）
fn read_monitor_list(file_name: &str) -> Vec<SelfDefineMonitorConfig> {
    match tools::read_json_file(file_name) {
        Ok(list) => list,
        Err(err) => {
            eprintln!("Error reading monitor list file: {}", err);
            vec![]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 从JSON构造配置项（与API请求体/monitor_list.json的实际入参路径一致）
    fn entry_from_json(json: &str) -> SelfDefineMonitorConfig {
        serde_json::from_str(json).expect("测试JSON应可反序列化")
    }

    #[test]
    fn http_get_params_are_appended_as_query() {
        let entry = entry_from_json(
            r#"{"target":"https://a.com/p","monitor_type":"HTTP","method":"GET","params":{"k":"v"}}"#,
        );
        let config = build_monitor_config(&entry, 5);
        let MonitorConfigDetail::Http(http) = config.details else {
            panic!("HTTP配置应生成Http详情");
        };
        assert_eq!(http.url, "https://a.com/p?k=v");
        assert!(matches!(http.method, HttpMethodTypes::Get));
    }

    #[test]
    fn http_post_params_become_json_body() {
        let entry = entry_from_json(
            r#"{"target":"https://a.com/p","monitor_type":"HTTP","method":"POST","params":{"k":"v"}}"#,
        );
        let config = build_monitor_config(&entry, 5);
        let MonitorConfigDetail::Http(http) = config.details else {
            panic!("HTTP配置应生成Http详情");
        };
        match http.body.expect("POST的params应转为JSON body") {
            HttpBody::Json(v) => assert_eq!(v["k"], "v"),
            other => panic!("应为Json body，实际: {:?}", other),
        }
    }

    #[test]
    fn explicit_text_body_overrides_params() {
        let entry = entry_from_json(
            r#"{"target":"https://a.com/p","monitor_type":"HTTP","method":"POST","params":{"k":"v"},"body":{"type":"text","content":"hi"}}"#,
        );
        let config = build_monitor_config(&entry, 5);
        let MonitorConfigDetail::Http(http) = config.details else {
            panic!("HTTP配置应生成Http详情");
        };
        match http.body.expect("显式body应生效") {
            HttpBody::Text(content) => assert_eq!(content, "hi"),
            other => panic!("应为Text body，实际: {:?}", other),
        }
        // 未显式配置Content-Type时默认补text/plain
        let headers = http.headers.expect("应补充默认Content-Type头");
        assert_eq!(
            headers.get(CONTENT_TYPE),
            Some(&HeaderValue::from_static("text/plain"))
        );
    }

    #[test]
    fn invalid_binary_body_is_dropped() {
        let entry = entry_from_json(
            r#"{"target":"https://a.com/p","monitor_type":"HTTP","method":"POST","body":{"type":"binary","content":"!!not_base64!!"}}"#,
        );
        let config = build_monitor_config(&entry, 5);
        let MonitorConfigDetail::Http(http) = config.details else {
            panic!("HTTP配置应生成Http详情");
        };
        // base64解码失败：不携带body，避免发出脏数据
        assert!(http.body.is_none());
    }

    #[test]
    fn interval_and_timeout_defaults_apply() {
        let entry = entry_from_json(r#"{"target":"https://a.com","monitor_type":"HTTP"}"#);
        let config = build_monitor_config(&entry, 7);
        assert_eq!(config.interval, Some(7));
        let MonitorConfigDetail::Http(http) = config.details else {
            panic!("HTTP配置应生成Http详情");
        };
        assert_eq!(http.timeout, 5000);
        assert!(http.headers.is_none());
    }

    #[test]
    fn unknown_type_falls_back_to_unknown_detail() {
        let entry = entry_from_json(r#"{"target":"x","monitor_type":"UNKNOWN"}"#);
        let config = build_monitor_config(&entry, 5);
        match config.details {
            MonitorConfigDetail::Unknown(u) => assert_eq!(u.description, "未知监控类型"),
            other => panic!("应为Unknown详情，实际: {:?}", other),
        }
    }
}
