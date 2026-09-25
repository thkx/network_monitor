// 日志模块
mod csv_logger;
use csv_logger::{CsvLogger, UrlLogResult};

// 结构化日志初始化模块（tracing：控制台 + 按日滚动文件）
mod logging;

// Prometheus指标模块（内存注册表 + /metrics 文本渲染）
mod metrics;
use metrics::MetricsRegistry;

// API认证模块（env凭证 + 内存会话 + actix中间件）
mod auth;
use auth::{Auth, AuthConfig, SessionStore};

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
use database::pool::establish_database_connection;
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
use monitor::types::{CheckResult, CheckResultDetail, MonitorConfig};

// 工具模块
mod tools;

// 全局通用的领域类型定义模块（输入配置 / 探测结果 / 告警通知）
mod domain;
use domain::{MonitorDefinition, display_name};

use actix_web::{App, HttpServer, web};
use clap::Parser;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

#[actix_web::main]
async fn main() {
    // 读取 .env（如 DATABASE_URL）；没有 .env 或读取失败时静默跳过，使用真实环境变量或代码内默认值
    dotenvy::dotenv().ok();
    // 初始化结构化日志（级别RUST_LOG，文件输出logs/按日滚动）；Guard保活到main结束以刷新文件缓冲
    let _log_guard = logging::init();
    tracing::info!("网络监控器 启动...");
    // 初始化日志结构体（CSV路径可用CSV_PATH覆盖，容器部署指向持久卷）
    let logger = CsvLogger::new(&csv_log_path());
    // 获取待监控的列表（JSON配置文件，每个监控引擎的参数都是一个对象，方便后续通过页面来配置）
    let monitor_list = read_monitor_list("monitor_list.json");
    // 获取命令行参数
    let args = Args::parse();
    // 匹配命令行参数
    match args.command {
        Commands::Once => {
            if monitor_list.is_empty() {
                tracing::warn!("监控列表为空，请在 monitor_list.json 中添加监控配置。");
                return;
            }
            // 对每个监控配置执行一次监控
            for entry in &monitor_list {
                let name = display_name(entry);
                let config = MonitorConfig::from_entry(entry, 5);
                let monitor = MonitorFactory::create_monitor(config.monitor_type);
                let mut rx = AsyncMonitor::create_once_monitoring(monitor, config).await;
                if let Some(result) = rx.recv().await {
                    log_result(&logger, &name, &result);
                    // 命中告警规则时通过告警引擎发送通知
                    if let Some(alert_rules) = entry.alert_rules.as_ref() {
                        let mut engine = AlertsEngine::new(alert_rules.clone());
                        if let Err(e) = engine.check(&result).await {
                            tracing::error!("告警检查失败: {}", e);
                        }
                    }
                }
            }
        }
        Commands::Monitor { interval } => {
            if monitor_list.is_empty() {
                tracing::warn!("监控列表为空，请在 monitor_list.json 中添加监控配置。");
                return;
            }
            // 所有监控任务共享一个通道，结果由单一消费者顺序处理，避免多通道轮询消费互相阻塞
            let (tx, mut rx) = mpsc::channel::<MonitorResultMessage>(100);
            for entry in &monitor_list {
                let name = display_name(entry);
                let config = MonitorConfig::from_entry(entry, interval);
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
        Commands::Server { port, interval } => {
            // Server模式：数据持久化 + 调度监控 + 告警 + Web API 的功能闭环
            run_server(port, interval, monitor_list).await;
        }
    }
}

// Server命令入口：
// 1.建立数据库连接 2.导入monitor_list.json 3.从数据库加载配置并调度监控
// 4.后台消费监控结果（CSV日志+数据库持久化+告警） 5.启动Web API服务
// default_interval：配置项未指定interval时的默认监控间隔（--interval，秒），
// 调度任务与手动执行接口共用同一来源（此前两处各自写死5，且Server模式无法配置）
async fn run_server(port: u16, default_interval: u64, monitor_list: Vec<MonitorDefinition>) {
    // 1. 建立数据库连接（带重试，自动执行迁移建表）
    let pool = match establish_database_connection().await {
        Ok(pool) => pool,
        Err(e) => {
            tracing::error!("数据库连接失败: {}", e);
            // 非0退出：容器/编排器（systemd、k8s、compose restart:on-failure）据此重启。
            // 此前仅 return，进程以 0 退出被误判为"正常结束"，不会触发重启。
            std::process::exit(1);
        }
    };
    let monitor_service = MonitorService::new(MonitorRepository::new(pool.clone()));
    let result_service = ResultService::new(CheckResultRepository::new(pool.clone()));

    // 2. 将 monitor_list.json 中的配置导入数据库（按名称去重，幂等）
    import_monitor_list(&monitor_service, &monitor_list, default_interval);

    // 2.5 数据保留策略：启动时清理一次过期监控结果，此后每24小时重复
    // 保留天数通过环境变量 RESULT_RETENTION_DAYS 配置（.env），默认30天
    let retention_days = std::env::var("RESULT_RETENTION_DAYS")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(30);
    let result_service_for_cleanup = result_service.clone();
    tokio::spawn(async move {
        loop {
            // 同步diesel DELETE挪到阻塞池执行，避免清理期间的SQLite写卡住runtime线程
            let svc = result_service_for_cleanup.clone();
            let days = retention_days;
            match tokio::task::spawn_blocking(move || svc.delete_expired(days)).await {
                Ok(Ok(n)) if n > 0 => {
                    tracing::info!("已清理 {} 天前的监控结果: {} 条", days, n)
                }
                Ok(Ok(_)) => {}
                Ok(Err(e)) => tracing::error!("清理过期监控结果失败: {}", e),
                Err(e) => tracing::error!("清理任务执行失败: {}", e),
            }
            tokio::time::sleep(std::time::Duration::from_secs(24 * 60 * 60)).await;
        }
    });

    // 指标注册表：消费者写入、/metrics端点读取（Server模式专属）
    let metrics_registry = Arc::new(Mutex::new(MetricsRegistry::new()));

    // 3. 后台任务持续消费监控结果：写CSV日志 + 持久化到数据库 + 更新Prometheus指标
    // （告警检查已下沉到各监控任务）
    // 句柄保留：优雅关停时等待消费者完成最终攒批落库
    let (tx, rx) = mpsc::channel::<MonitorResultMessage>(100);
    let result_service_for_loop = result_service.clone();
    let metrics_for_loop = metrics_registry.clone();
    let consumer_handle = tokio::spawn(consume_results(
        result_service_for_loop,
        rx,
        metrics_for_loop,
    ));

    // 4. 调度器加载启用配置并启动定时监控；API 增删改配置后可整体重建实现热更新
    let mut scheduler = Scheduler::new(pool.clone(), default_interval);
    scheduler.set_sender(tx);
    scheduler.reload_all(&monitor_service);
    let scheduler = web::Data::new(Arc::new(Mutex::new(scheduler)));

    // 5. 启动Web API服务
    // 认证：ADMIN_PASSWORD配置时启用会话认证，未配置时开放并明确WARN
    let auth_config = Arc::new(AuthConfig::from_env());
    if auth_config.enabled {
        tracing::info!("API认证已启用 (user: {})", auth_config.username);
    } else {
        tracing::warn!("ADMIN_PASSWORD 未设置：API与控制台处于无认证状态，请勿暴露到公网");
    }
    let session_store = Arc::new(Mutex::new(SessionStore::new()));
    // 会话/限流表周期清扫：过期会话与失效失败记录不残留，防内存无界增长
    // （IPv6轮换等攻击制造的失败记录受attempts容量上限保护，见SessionStore::sweep）
    let session_store_for_sweep = session_store.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(60));
        loop {
            tick.tick().await;
            session_store_for_sweep
                .lock()
                .expect("session锁中毒")
                .sweep();
        }
    });
    let metrics_for_app = metrics_registry.clone();
    let pool_for_app = pool.clone();
    // 监听地址：默认127.0.0.1仅本机可访问；容器/局域网部署时设BIND_ADDR=0.0.0.0
    let bind_addr = std::env::var("BIND_ADDR").unwrap_or_else(|_| "127.0.0.1".to_string());
    // 先绑定并转成Server future：ServerHandle从Server获取（actix-web 4中HttpServer本身没有handle()）
    // 关停段持有独立的Data克隆：HttpServer闭包move会接管scheduler本体
    let scheduler_for_shutdown = scheduler.clone();
    let server = match HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(monitor_service.clone()))
            .app_data(web::Data::new(result_service.clone()))
            .app_data(web::Data::new(metrics_for_app.clone()))
            .app_data(web::Data::new(pool_for_app.clone()))
            // scheduler已是Data<Arc<Mutex<Scheduler>>>，直接注册；再包一层会导致handler提取类型不匹配(500)
            .app_data(scheduler.clone())
            .app_data(web::Data::new(default_interval))
            .app_data(web::Data::new(session_store.clone()))
            .app_data(web::Data::new(auth_config.clone()))
            .wrap(Auth::new(session_store.clone(), auth_config.clone()))
            .configure(api::configure_routes)
    })
    .bind((bind_addr.as_str(), port))
    {
        Ok(srv) => srv.run(),
        Err(e) => {
            tracing::error!("Web API 端口 {} 绑定失败: {}", port, e);
            // 非0退出：端口被占用/权限不足属启动失败，编排器据此重启（见上方DB失败说明）。
            std::process::exit(1);
        }
    };
    tracing::info!("Web API 已启动: http://127.0.0.1:{}/api/monitors", port);
    // 优雅关停：监听退出信号（Ctrl+C / Unix SIGTERM），
    // stop(true)让Server停止接受新请求并等待进行中的请求完成
    let server_handle = server.handle();
    tokio::spawn(async move {
        shutdown_signal().await;
        tracing::info!("收到退出信号，开始优雅关停：停止接受新请求…");
        server_handle.stop(true).await;
    });
    if let Err(e) = server.await {
        tracing::error!("Web API 服务异常退出: {}", e);
    }
    // 服务器退出后：停止全部监控任务并释放调度器持有的发送端；
    // 发送端全部drop后消费者的recv返回None，触发最终攒批落库后退出
    if let Ok(mut sched) = scheduler_for_shutdown.lock() {
        sched.shutdown();
        tracing::info!("已停止全部定时监控任务");
    }
    match tokio::time::timeout(std::time::Duration::from_secs(10), consumer_handle).await {
        Ok(_) => tracing::info!("结果消费者已完成最终落库，进程退出"),
        Err(_) => tracing::warn!("等待消费者收尾超时（10s），强制退出"),
    }
}

// 阻塞直到收到退出信号：Ctrl+C，Unix下另监听SIGTERM
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
            }
            Err(e) => {
                tracing::warn!("无法监听SIGTERM（{}），仅Ctrl+C生效", e);
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

// 消费者攒批参数：满64条、或首个待写结果等待超过1秒，即批量落库
// （监控间隔普遍≥5秒，1秒的入库延迟对状态展示无感；换取写锁次数降为批次级）
const BATCH_MAX_ITEMS: usize = 64;
const BATCH_FLUSH_MILLIS: u64 = 1000;

// 持续接收各监控任务的结果：更新指标、写CSV、攒批持久化数据库
// 共享通道 + 单一消费者：所有监控任务的结果在此顺序处理，互不阻塞
// 告警检查已下沉到各监控任务内部，由任务独占引擎与抑制状态
async fn consume_results(
    result_service: ResultService,
    mut rx: mpsc::Receiver<MonitorResultMessage>,
    metrics: Arc<Mutex<MetricsRegistry>>,
) {
    let logger = CsvLogger::new(&csv_log_path());
    let mut buffer: Vec<MonitorResultMessage> = Vec::with_capacity(BATCH_MAX_ITEMS);
    let mut flush_tick =
        tokio::time::interval(std::time::Duration::from_millis(BATCH_FLUSH_MILLIS));
    flush_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            message = rx.recv() => {
                let Some(message) = message else {
                    // 所有发送端关闭（优雅关停路径）：清空余量后结束
                    flush_batch_async(&result_service, &mut buffer).await;
                    break;
                };
                // Prometheus指标更新（Once/Monitor模式无monitor_id时自动跳过）
                {
                    let (status, response_time, _code) = message.result.log_fields();
                    let response_time_ms = u64::try_from(response_time).unwrap_or(u64::MAX);
                    metrics
                        .lock()
                        .expect("metrics锁中毒")
                        .record(
                            &message.route,
                            &message.result.monitor_type.to_string(),
                            status,
                            response_time_ms,
                        );
                }
                log_result(&logger, &message.route.name, &message.result);
                // 数据库持久化走攒批；无主键的结果（Once/Monitor模式）本就不入库
                if message.route.monitor_id.is_some() {
                    buffer.push(message);
                    if buffer.len() >= BATCH_MAX_ITEMS {
                        flush_batch_async(&result_service, &mut buffer).await;
                    }
                }
            }
            _ = flush_tick.tick() => {
                flush_batch_async(&result_service, &mut buffer).await;
            }
        }
    }
}

// 把攒批的检查结果一次性落库（多行INSERT）；批量失败降级为逐条写入保数据
fn flush_batch(result_service: &ResultService, buffer: &mut Vec<MonitorResultMessage>) {
    if buffer.is_empty() {
        return;
    }
    // (monitor_id, result)引用对：入队前已过滤monitor_id.is_some()
    let pairs: Vec<(i32, &CheckResult)> = buffer
        .iter()
        .filter_map(|m| m.route.monitor_id.map(|id| (id, &m.result)))
        .collect();
    let count = pairs.len();
    match result_service.persist_checks_batch(&pairs) {
        Ok(_) => tracing::debug!("批量持久化 {} 条监控结果", count),
        Err(e) => {
            // 降级逐条写入：定位问题行，其余数据不丢（CSV日志另有完整备份）
            tracing::error!("批量持久化失败({})，降级逐条写入: {} 条", e, count);
            for (monitor_id, result) in pairs {
                if let Err(e) = result_service.persist_check(monitor_id, result) {
                    tracing::error!("单条持久化失败 (monitor_id={}): {}", monitor_id, e);
                }
            }
        }
    }
    buffer.clear();
}

// 攒批落库的异步包装：flush_batch内的批量INSERT是同步阻塞调用，
// 在async消费者任务内直接执行会卡住runtime线程，挪到spawn_blocking阻塞池。
// 缓冲区经mem::take移入阻塞任务：正常完成时flush_batch已清空并带回空缓冲；
// Join失败（阻塞任务panic）时数据仍在移入的副本里，随JoinError丢失——
// 因此从JoinError的panic载荷无法取回，改为在移入前保留条数用于日志，
// 并把未落库数据的丢失显式记录（CSV日志另有完整备份，可据此人工补录）
async fn flush_batch_async(result_service: &ResultService, buffer: &mut Vec<MonitorResultMessage>) {
    if buffer.is_empty() {
        return;
    }
    let svc = result_service.clone();
    let taken = std::mem::take(buffer);
    let dropped_count = taken.len();
    match tokio::task::spawn_blocking(move || {
        let mut buf = taken;
        flush_batch(&svc, &mut buf);
        buf // flush_batch末尾clear，正常路径带回空Vec
    })
    .await
    {
        Ok(buf) => *buffer = buf,
        // 阻塞任务panic：移入的数据随JoinError丢失，无法取回。
        // 显式告警而非静默吞掉——这批结果在DB里缺失，需依赖CSV日志人工核对
        Err(e) => {
            tracing::error!(
                "攒批落库任务panic，{}条结果未能写入数据库（CSV日志仍有记录，可据此核对）: {}",
                dropped_count,
                e
            );
        }
    }
}

// 把JSON配置导入monitor_config表（name有唯一约束，按名称去重保证幂等）
fn import_monitor_list(
    monitor_service: &MonitorService,
    monitor_list: &[MonitorDefinition],
    default_interval: u64,
) {
    for entry in monitor_list {
        let name = display_name(entry);
        match monitor_service.find_by_name(&name) {
            Ok(Some(_)) => continue, // 已存在，跳过
            Ok(None) => {
                let insert = build_monitor_insert(entry, default_interval);
                match monitor_service.create_monitor(&insert) {
                    Ok(_) => tracing::info!("监控配置已导入: {}", name),
                    Err(e) => tracing::error!("监控配置导入失败 {}: {}", name, e),
                }
            }
            Err(e) => tracing::error!("查询监控配置 {} 失败: {}", name, e),
        }
    }
}

// 将监控结果写入CSV日志并输出结构化日志
// 级别设计：成功为debug（默认info不显示，RUST_LOG=debug可看全部）、失败为warn（始终可见）
fn log_result(logger: &CsvLogger, name: &str, result: &CheckResult) {
    let (status, response_time, status_code) = result.log_fields();
    // 兜底类型的诊断信息：把Unknown的description暴露出来，说明为什么走到了兜底
    if let CheckResultDetail::Unknown(u) = &result.details {
        tracing::warn!("{}", u.description);
    }
    // 检查ID以16进制输出，用于跨CSV日志与数据库关联同一次检查
    let check_id = format!("{:x}", result.id);
    if let Err(e) = logger.log(UrlLogResult {
        check_id: check_id.clone(),
        url: name.to_string(),
        status,
        response_time,
        status_code,
    }) {
        // CSV写入失败不再panic中断消费循环，降级为错误日志（数据库仍有完整结果）
        tracing::error!("CSV日志写入失败: {}", e);
    }
    let response_time_ms = u64::try_from(response_time).unwrap_or(u64::MAX);
    if status {
        tracing::debug!(
            check_id = %check_id,
            monitor = %name,
            response_time_ms,
            status_code = ?status_code,
            "检查完成：可用"
        );
    } else {
        tracing::warn!(
            check_id = %check_id,
            monitor = %name,
            response_time_ms,
            status_code = ?status_code,
            "检查完成：不可用"
        );
    }
}

// CSV日志路径：环境变量CSV_PATH覆盖，缺省当前目录（容器部署应指向持久卷）
fn csv_log_path() -> String {
    std::env::var("CSV_PATH").unwrap_or_else(|_| "monitor_log.csv".to_string())
}

// 获取监控的列表（从JSON配置文件读取，每个监控引擎的参数都是一个对象）
fn read_monitor_list(file_name: &str) -> Vec<MonitorDefinition> {
    match tools::read_json_file(file_name) {
        Ok(list) => list,
        Err(err) => {
            tracing::warn!("读取监控列表文件失败: {}", err);
            vec![]
        }
    }
}
