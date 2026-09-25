use super::Monitor;
use super::types::{CheckResultDetail, MonitorConfig, TracerouteMonitorResult};
use crate::domain::MonitorType;
use std::process::Stdio;
use std::time::Duration;
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
        // 外层兜底超时：15跳×每跳3探测×1秒≈45秒是正常上界，取max(config.timeout, 60秒)，
        // 只防子进程异常挂起（此前output().await无兜底，挂起会让任务永久失明）
        let guard = Duration::from_millis(config.timeout.max(60_000));
        // 路由追踪：Windows用tracert；Unix优先tracepath（免安装、免root），命令不存在时退回traceroute
        let mut success = false;
        let mut hops: Vec<String> = vec![];
        let mut cmd = if cfg!(target_os = "windows") {
            let mut c = Command::new("tracert");
            c.args(["-d", "-h", "15", "-w", "1000"]);
            c
        } else {
            let mut c = Command::new("tracepath");
            c.args(["-m", "15"]);
            c
        };
        cmd.arg(target.as_str())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true); // 外层超时取消future时子进程随之被kill
        let spawned = cmd.spawn();
        // spawn失败且是命令不存在：Unix上回退traceroute重试一次（同样带kill_on_drop与兜底超时）
        let output = match spawned {
            Ok(child) => tokio::time::timeout(guard, child.wait_with_output())
                .await
                .ok()
                .and_then(|r| r.ok()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // traceroute 参数：-n 不解析域名，-m 15 最大跃点数，-w 1 每跳超时1秒
                let fallback = Command::new("traceroute")
                    .args(["-n", "-m", "15", "-w", "1"])
                    .arg(target.as_str())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .kill_on_drop(true)
                    .spawn();
                match fallback {
                    Ok(child) => tokio::time::timeout(guard, child.wait_with_output())
                        .await
                        .ok()
                        .and_then(|r| r.ok()),
                    Err(_) => None,
                }
            }
            Err(_) => None,
        };
        if let Some(output) = output {
            // 逐行解析输出，保留以数字开头的跃点行（跳过头部说明和空行）
            let text = String::from_utf8_lossy(&output.stdout);
            for line in text.lines() {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if let Some(first) = trimmed.chars().next()
                    && first.is_ascii_digit()
                {
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
