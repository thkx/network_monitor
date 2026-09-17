use super::monitor_trait::Monitor;
use super::types::{CheckResultDetail, IcmpMonitorResult, MonitorConfig};
use crate::tools_types::MonitorType;
use std::process::Stdio;
use std::time::{Duration, Instant};
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
        // 探测超时对齐config.timeout（毫秒，API校验1~300000；下限1秒防配置0导致永远超时）
        let timeout_ms = config.timeout.max(1000);
        // 通过调用系统自带的ping命令实现ICMP探测（避免裸套接字需要管理员权限的问题）
        // 只发送1次探测包；单包等待时间与config.timeout对齐（此前硬编码3秒，配置的超时不生效）
        // Windows -w毫秒 / Linux -W秒(向上取整) / macOS -W毫秒
        #[cfg(target_os = "windows")]
        let ping_args: Vec<String> =
            vec!["-n".into(), "1".into(), "-w".into(), timeout_ms.to_string()];
        #[cfg(target_os = "macos")]
        let ping_args: Vec<String> =
            vec!["-c".into(), "1".into(), "-W".into(), timeout_ms.to_string()];
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        let ping_args: Vec<String> = vec![
            "-c".into(),
            "1".into(),
            "-W".into(),
            ((timeout_ms + 999) / 1000).to_string(),
        ];
        // spawn + kill_on_drop + 外层tokio超时：超时取消future时子进程随之被kill
        // （此前output().await无兜底超时，ping异常挂起会让任务永久失明且无任何报错）
        let spawned = Command::new("ping")
            .args(&ping_args)
            .arg(target.as_str())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn();
        let output = match spawned {
            Ok(child) => tokio::time::timeout(Duration::from_millis(timeout_ms), child.wait_with_output())
                .await
                .ok()
                .and_then(|r| r.ok()),
            Err(_) => None, // ping命令不存在等启动失败：按不可用处理
        };
        let elapsed_ms = start.elapsed().as_millis();
        // ping命令执行成功（退出码为0）即代表目标主机存活；超时/启动失败均不可用
        let is_alive = matches!(output, Some(o) if o.status.code() == Some(0));
        (
            true,
            CheckResultDetail::Icmp(IcmpMonitorResult { is_alive, elapsed_ms }),
        )
    }

    fn get_type(&self) -> MonitorType {
        MonitorType::Icmp
    }
}
