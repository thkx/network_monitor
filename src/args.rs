use clap::{Parser, Subcommand};
#[derive(Parser)]
#[command(name = "网络监控器", about = "一个简单的网络监控工具")]
pub struct Args {
    // 命令行参数
    // subcommand:代表该字段是`子命令`的容器，clap会根据命令行把对应的子命令解析为该字段的枚举值变体
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    Once, //    cargo run -- Once
    Monitor {
        // 检查间隔
        #[arg(short, long, default_value = "5")]
        interval: u64, // cargo run -- Monitor --interval 10
    },
    // 启动完整服务：导入配置→调度监控→结果持久化→告警→Web API
    Server {
        // Web API 监听端口
        #[arg(short, long, default_value = "8080")]
        port: u16, // cargo run -- Server --port 8080
        // 默认监控间隔（秒）：配置项未指定interval时使用
        #[arg(short, long, default_value = "5")]
        interval: u64, // cargo run -- Server --interval 30
    },
}
