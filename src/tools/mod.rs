// 工具模块：提供JSON配置读取、目标地址解析等辅助函数
pub mod http;
pub use http::{get_dns_tcp_tls_performance, parse_host_port};

pub mod file;
pub use file::read_json_file;

pub mod retry;
pub use retry::default_retry_policy;

pub mod smtp;
pub use smtp::send_mail;
