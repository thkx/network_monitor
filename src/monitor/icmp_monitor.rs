use super::monitor_trait::Monitor;
use super::types::{CheckResultDetail, IcmpMonitorResult, MonitorConfig};
use crate::tools_types::MonitorType;
use std::time::Instant;
use tokio::process::Command;

pub struct IcmpMonitor {}

impl IcmpMonitor {
    pub fn new() -> Self {
        IcmpMonitor {}
    }
}

// 用来让trait中的异步方法可用，详细的请查看Q&A
#[async_trait::async_trait]
impl Monitor for IcmpMonitor {
    async fn check(&self, config: &MonitorConfig) -> (bool, CheckResultDetail) {
        // 判断一下是否存在 target 此时的target 应该是一个 主机地址或域名
        if config.target.is_none() {
            return (false, CheckResultDetail::Icmp(IcmpMonitorResult::default()));
        }
        let target = config.target.as_ref().unwrap();
        let start = Instant::now();
        // 通过调用系统自带的ping命令实现ICMP探测（避免裸套接字需要管理员权限的问题）
        // 只发送1次探测包；超时参数按平台区分（Windows -w毫秒 / Linux -W秒 / macOS -W毫秒）
        #[cfg(target_os = "windows")]
        let ping_args: [&str; 4] = ["-n", "1", "-w", "3000"];
        #[cfg(target_os = "linux")]
        let ping_args: [&str; 4] = ["-c", "1", "-W", "3"];
        #[cfg(target_os = "macos")]
        let ping_args: [&str; 4] = ["-c", "1", "-W", "3000"];
        #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
        let ping_args: [&str; 4] = ["-c", "1", "-W", "3"];
        let output = Command::new("ping")
            .args(ping_args)
            .arg(target.as_str())
            .output()
            .await;
        let elapsed_ms = start.elapsed().as_millis();
        // ping命令执行成功（退出码为0）即代表目标主机存活
        let is_alive = matches!(output, Ok(o) if o.status.code() == Some(0));
        (
            true,
            CheckResultDetail::Icmp(IcmpMonitorResult { is_alive, elapsed_ms }),
        )
    }

    fn get_type(&self) -> MonitorType {
        MonitorType::Icmp
    }
}
