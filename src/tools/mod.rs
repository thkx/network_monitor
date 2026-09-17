// 工具模块：提供JSON配置读取、目标地址解析等辅助函数
pub mod http_tool;
pub use http_tool::{get_dns_tcp_tls_performance, parse_host_port};

pub mod file_tool;
pub use file_tool::read_json_file;

pub mod retry_tool;
pub use retry_tool::default_retry_policy;

pub mod smtp_tool;
pub use smtp_tool::send_mail;
