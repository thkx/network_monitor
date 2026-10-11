// 集中运行配置（ADR-0008 D3）：进程启动时一次性从环境读取，作为单一事实来源。
// 动机：同一环境变量此前在多处各自 env::var 读取，存在漂移风险——典型是 BIND_ADDR
// 既被 main 用于实际绑定端口，又被 auth 用于推断 cookie 的 Secure 属性，两处各读一次，
// 一旦取值逻辑/默认值分叉，就会出现"绑定地址与 Secure 推断依据不一致"的隐患。
// 现在集中到 AppConfig 读取一次，再把同一个值分发给各装配点。
pub struct AppConfig {
    // 监听地址：默认 127.0.0.1 仅本机可访问；容器/局域网部署设 BIND_ADDR=0.0.0.0
    pub bind_addr: String,
    // CSV 日志基础路径（CSV_PATH，容器部署应指向持久卷）
    pub csv_path: String,
    // 监控结果保留天数（RESULT_RETENTION_DAYS，默认 30）
    pub result_retention_days: i64,
}

impl AppConfig {
    pub fn from_env() -> Self {
        let bind_addr = std::env::var("BIND_ADDR").unwrap_or_else(|_| "127.0.0.1".to_string());
        let csv_path = std::env::var("CSV_PATH").unwrap_or_else(|_| "monitor_log.csv".to_string());
        let result_retention_days = std::env::var("RESULT_RETENTION_DAYS")
            .ok()
            .and_then(|v| v.trim().parse::<i64>().ok())
            .unwrap_or(30);
        AppConfig {
            bind_addr,
            csv_path,
            result_retention_days,
        }
    }
}

// default_interval 的依赖注入 newtype（ADR-0008 D4）：
// actix 的 web::Data 以类型为键，直接用裸 u64 作键，一旦有第二个 u64 被注入就会相互覆盖/错配，
// 且只在运行期提取时暴露为 500。包一层 newtype 后，装配错配变为编译期可查，语义也更清晰。
#[derive(Debug, Clone, Copy)]
pub struct DefaultInterval(pub u64);
