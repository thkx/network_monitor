use super::monitor_trait::Monitor;
use super::types::{CheckResultDetail, MonitorConfig, TracerouteMonitorResult};
use crate::tools_types::MonitorType;
use tokio::process::Command;

pub struct TracerouteMonitor {}

impl TracerouteMonitor {
    pub fn new() -> Self {
        TracerouteMonitor {}
    }
}

// 用来让trait中的异步方法可用，详细的请查看Q&A
#[async_trait::async_trait]
impl Monitor for TracerouteMonitor {
    async fn check(&self, config: &MonitorConfig) -> (bool, CheckResultDetail) {
        // 判断一下是否存在 target 此时的target 应该是一个主机地址或域名
        if config.target.is_none() {
            return (
                false,
                CheckResultDetail::Traceroute(TracerouteMonitorResult::default()),
            );
        }
        let target = config.target.as_ref().unwrap();
        // 调用系统自带的 tracert 命令实现路由追踪（Windows平台参数）
        // -d 不对跳点做反向域名解析（加快速度），-h 15 最大跃点数，-w 1000 每次探测等待超时1秒
        // 路由追踪：Windows用tracert；Unix优先tracepath（免安装、免root），命令不存在时退回traceroute
        let mut success = false;
        let mut hops: Vec<String> = vec![];
        let output = if cfg!(target_os = "windows") {
            // -d 不做反向域名解析，-h 15 最大跃点数，-w 1000 每次探测等待超时1秒
            Command::new("tracert")
                .args(["-d", "-h", "15", "-w", "1000"])
                .arg(target.as_str())
                .output()
                .await
        } else {
            match Command::new("tracepath")
                .args(["-m", "15"])
                .arg(target.as_str())
                .output()
                .await
            {
                Ok(o) => Ok(o),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    // traceroute 参数：-n 不解析域名，-m 15 最大跃点数，-w 1 每跳超时1秒
                    Command::new("traceroute")
                        .args(["-n", "-m", "15", "-w", "1"])
                        .arg(target.as_str())
                        .output()
                        .await
                }
                Err(e) => Err(e),
            }
        };
        if let Ok(output) = output {
            // 逐行解析输出，保留以数字开头的跃点行（跳过头部说明和空行）
            let text = String::from_utf8_lossy(&output.stdout);
            for line in text.lines() {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if let Some(first) = trimmed.chars().next()
                    && first.is_ascii_digit() {
                        hops.push(trimmed.to_string());
                    }
            }
            // 至少追踪到一个跃点即视为成功
            success = !hops.is_empty();
        }
        (
            true,
            CheckResultDetail::Traceroute(TracerouteMonitorResult { success, hops }),
        )
    }

    fn get_type(&self) -> MonitorType {
        MonitorType::Traceroute
    }
}
