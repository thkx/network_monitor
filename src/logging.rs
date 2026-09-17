// 日志初始化模块：tracing 双通道输出（控制台 + 按日滚动文件 logs/network_monitor.log）
// 级别通过 RUST_LOG 环境变量控制，缺省 info，且 hyper/reqwest 静音到 warn 防止HTTP库刷屏

use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::time::FormatTime;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

// 本地时间戳格式化器（复用已有chrono依赖，避免为此引入time crate）
struct LocalTimer;

impl FormatTime for LocalTimer {
    fn format_time(&self, w: &mut Writer<'_>) -> std::fmt::Result {
        write!(w, "{}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"))
    }
}

// 初始化全局日志订阅器；返回的 Guard 必须在 main 中保活（drop 时刷新文件缓冲区）
// 日志目录可用 LOG_DIR 覆盖（缺省 ./logs，容器部署应指向持久卷）
pub fn init() -> tracing_appender::non_blocking::WorkerGuard {
    let log_dir = std::env::var("LOG_DIR").unwrap_or_else(|_| "logs".to_string());
    let file_appender = tracing_appender::rolling::daily(&log_dir, "network_monitor.log");
    let (file_writer, guard) = tracing_appender::non_blocking(file_appender);

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,hyper=warn,reqwest=warn"));

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stdout)
                .with_timer(LocalTimer),
        )
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(file_writer)
                .with_ansi(false)
                .with_timer(LocalTimer),
        )
        .with(filter)
        .init();
    guard
}
