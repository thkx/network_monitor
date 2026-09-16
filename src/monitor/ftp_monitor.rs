use super::monitor_trait::Monitor;
use super::types::{CheckResultDetail, FtpMonitorResult, MonitorConfig};
use crate::tools::parse_host_port;
use crate::tools_types::MonitorType;
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;

pub struct FtpMonitor {}

impl FtpMonitor {
    pub fn new() -> Self {
        FtpMonitor {}
    }
}

// 用来让trait中的异步方法可用，详细的请查看Q&A
#[async_trait::async_trait]
impl Monitor for FtpMonitor {
    async fn check(&self, config: &MonitorConfig) -> (bool, CheckResultDetail) {
        // 判断一下是否存在 target 此时的target 应该是 host 或 host:port 格式的地址
        if config.target.is_none() {
            return (false, CheckResultDetail::Ftp(FtpMonitorResult::default()));
        }
        let target = config.target.as_ref().unwrap();
        // 解析目标地址，FTP默认端口为21
        let (host, port) = match parse_host_port(target, 21) {
            Some(v) => v,
            None => {
                // 地址格式不合法，返回失败结果
                return (false, CheckResultDetail::Ftp(FtpMonitorResult::default()));
            }
        };
        let start = Instant::now();
        let mut connected = false; // 是否连接成功
        let mut banner: Option<String> = None; // 服务端欢迎横幅
        // 带超时地发起TCP连接（超时时间5秒）
        let connect_res = tokio::time::timeout(
            Duration::from_secs(5),
            tokio::net::TcpStream::connect((host.as_str(), port)),
        )
        .await;
        if let Ok(Ok(mut stream)) = connect_res {
            connected = true;
            // FTP服务器在连接建立后会主动推送欢迎横幅，读取它来验证服务可用（最多等待3秒）
            let mut buf = [0u8; 512];
            if let Ok(Ok(n)) =
                tokio::time::timeout(Duration::from_secs(3), stream.read(&mut buf)).await
                && n > 0 {
                    banner = Some(String::from_utf8_lossy(&buf[..n]).trim().to_string());
                }
        }
        let elapsed_ms = start.elapsed().as_millis();
        (
            true,
            CheckResultDetail::Ftp(FtpMonitorResult {
                connected,
                banner,
                elapsed_ms,
            }),
        )
    }

    fn get_type(&self) -> MonitorType {
        MonitorType::Ftp
    }
}
