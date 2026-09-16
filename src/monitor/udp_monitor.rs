use super::monitor_trait::Monitor;
use super::types::{CheckResultDetail, MonitorConfig, UdpMonitorResult};
use crate::tools::parse_host_port;
use crate::tools_types::MonitorType;
use std::time::Instant;
use tokio::net::UdpSocket;

pub struct UdpMonitor {}

impl UdpMonitor {
    pub fn new() -> Self {
        UdpMonitor {}
    }
}

// 用来让trait中的异步方法可用，详细的请查看Q&A
#[async_trait::async_trait]
impl Monitor for UdpMonitor {
    async fn check(&self, config: &MonitorConfig) -> (bool, CheckResultDetail) {
        // 判断一下是否存在 target 此时的target 应该是 host:port 格式的地址
        if config.target.is_none() {
            return (false, CheckResultDetail::Udp(UdpMonitorResult::default()));
        }
        let target = config.target.as_ref().unwrap();
        // 解析目标地址，未指定端口时默认使用53端口（DNS服务端口）
        let (host, port) = match parse_host_port(target, 53) {
            Some(v) => v,
            None => {
                // 地址格式不合法，返回失败结果
                return (false, CheckResultDetail::Udp(UdpMonitorResult::default()));
            }
        };
        let start = Instant::now();
        let mut sent = false; // 是否成功发送探测包
        let mut response_received = false; // 是否收到响应
        // 先把目标地址解析为 SocketAddr
        if let Ok(mut addrs) = tokio::net::lookup_host((host.as_str(), port)).await
            && let Some(addr) = addrs.next() {
                // 绑定一个随机本地端口用于发送和接收
                if let Ok(socket) = UdpSocket::bind("0.0.0.0:0").await {
                    // 发送一个探测包
                    if socket.send_to(b"ping", addr).await.is_ok() {
                        sent = true;
                        let mut buf = [0u8; 1024];
                        // 等待响应，最多等待3秒
                        if let Ok(res) = tokio::time::timeout(
                            std::time::Duration::from_secs(3),
                            socket.recv_from(&mut buf),
                        )
                        .await
                        {
                            response_received = res.is_ok();
                        }
                    }
                }
            }
        let elapsed_ms = start.elapsed().as_millis();
        (
            true,
            CheckResultDetail::Udp(UdpMonitorResult {
                sent,
                response_received,
                elapsed_ms,
            }),
        )
    }

    fn get_type(&self) -> MonitorType {
        MonitorType::Udp
    }
}
