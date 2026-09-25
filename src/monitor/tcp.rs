use super::Monitor;
use super::types::{CheckResultDetail, MonitorConfig, TcpMonitorResult};
use crate::domain::MonitorType;
use crate::tools::parse_host_port;
use std::time::Instant;

pub struct TcpMonitor {}

impl TcpMonitor {
    pub fn new() -> Self {
        TcpMonitor {}
    }
}

// 用来让trait中的异步方法可用，详细的请查看Q&A
#[async_trait::async_trait]
impl Monitor for TcpMonitor {
    async fn check(&self, config: &MonitorConfig) -> (bool, CheckResultDetail) {
        // 判断一下是否存在 target 此时的target 应该是 host:port 格式的地址
        if config.target.is_none() {
            return (false, CheckResultDetail::Tcp(TcpMonitorResult::default()));
        }
        let target = config.target.as_ref().unwrap();
        // 解析目标地址，未指定端口时默认使用80端口
        let (host, port) = match parse_host_port(target, 80) {
            Some(v) => v,
            None => {
                // 地址格式不合法，返回失败结果
                return (false, CheckResultDetail::Tcp(TcpMonitorResult::default()));
            }
        };
        let start = Instant::now();
        // 带超时地发起TCP连接（超时时间5秒），避免单个地址阻塞过久
        let connect_res = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            tokio::net::TcpStream::connect((host.as_str(), port)),
        )
        .await;
        let elapsed_ms = start.elapsed().as_millis();
        // 连接成功即代表目标端口可用
        let connected = matches!(connect_res, Ok(Ok(_)));
        (
            true,
            CheckResultDetail::Tcp(TcpMonitorResult {
                connected,
                elapsed_ms,
            }),
        )
    }

    fn get_type(&self) -> MonitorType {
        MonitorType::Tcp
    }
}
