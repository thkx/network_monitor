use super::Monitor;
use crate::domain::MonitorType;
use crate::domain::{CheckResultDetail, MonitorConfig, TcpMonitorResult};
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
        // 连接超时对齐 config.timeout（毫秒；下限1秒防配置0导致立即超时，与ICMP/UDP同规则）
        // 此前硬编码5秒，配置的超时对TCP不生效
        let connect_res = tokio::time::timeout(
            std::time::Duration::from_millis(config.timeout.max(1000)),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{MonitorConfig, MonitorConfigDetail, TcpMonitorConfig};

    // 连接超时对齐 config.timeout：连一个不可路由地址（RFC 5737 TEST-NET-1），
    // timeout=1000 时应在远小于旧硬编码5秒内返回失败（证明配置生效）。
    #[tokio::test]
    async fn connect_timeout_honors_config_timeout() {
        let config = MonitorConfig {
            target: Some("192.0.2.1:80".to_string()),
            interval: Some(60),
            monitor_type: MonitorType::Tcp,
            timeout: 1000,
            details: MonitorConfigDetail::Tcp(TcpMonitorConfig {}),
        };
        let started = Instant::now();
        let (status, detail) = TcpMonitor::new().check(&config).await;
        let wall = started.elapsed();
        assert!(status, "检查本身应完成");
        let CheckResultDetail::Tcp(r) = detail else {
            panic!("应为TCP结果");
        };
        assert!(!r.connected, "不可路由地址不应连接成功");
        // 上界给足余量防CI抖动，但必须显著小于旧硬编码5秒——证明超时对齐了config.timeout
        assert!(
            wall < std::time::Duration::from_secs(3),
            "应在~1s（config.timeout）而非旧5秒内超时，实际 {:?}",
            wall
        );
    }
}
