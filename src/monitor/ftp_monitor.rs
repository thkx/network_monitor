use super::monitor_trait::Monitor;
use super::types::{CheckResultDetail, FtpMonitorResult, MonitorConfig};
use crate::tools::parse_host_port;
use crate::tools_types::MonitorType;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

pub struct FtpMonitor {}

impl FtpMonitor {
    pub fn new() -> Self {
        FtpMonitor {}
    }
}

// FTP监控为真实协议探测：连接后完成一次匿名登录握手（RFC 959）：
//   读取220横幅 → USER anonymous → （331时）PASS … → 最终响应码
// 每一步都要求合法的三位FTP响应码，可区分"真FTP服务"与"恰好开了21端口的任意TCP服务"；
// 服务器拒绝匿名登录（如530）时握手仍算完成（协议功能正常），logged_in=false如实记录。
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
        let mut result = FtpMonitorResult::default();
        // 带超时地发起TCP连接（超时时间5秒）
        let connect_res = tokio::time::timeout(
            Duration::from_secs(5),
            TcpStream::connect((host.as_str(), port)),
        )
        .await;
        if let Ok(Ok(mut stream)) = connect_res {
            result.connected = true;
            let mut buf = Vec::with_capacity(256);
            // 步骤1：欢迎横幅（合法FTP服务以2xx响应码开始）
            if let Ok(code) = read_ftp_reply(&mut stream, &mut buf).await {
                result.last_code = Some(code);
                result.banner = Some(String::from_utf8_lossy(&buf).trim().to_string());
                // 横幅字节清空：read_ftp_reply对累积缓冲解析，残留横幅会被误当下一步响应
                buf.clear();
                if code == 220 {
                    // 步骤2：USER anonymous
                    if send_cmd(&mut stream, "USER anonymous").await.is_ok()
                        && let Ok(c1) = read_ftp_reply(&mut stream, &mut buf).await
                    {
                        result.last_code = Some(c1);
                        result.handshake_ok = true;
                        match c1 {
                            // 免密直接登录成功
                            230 => result.logged_in = true,
                            // 需要密码：步骤3 PASS（密码用占位邮箱，探测不假设真实凭证）
                            331 => {
                                buf.clear();
                                if send_cmd(&mut stream, "PASS ftp-monitor@example.com")
                                    .await
                                    .is_ok()
                                    && let Ok(c2) = read_ftp_reply(&mut stream, &mut buf).await
                                {
                                    result.last_code = Some(c2);
                                    result.logged_in = (200..=299).contains(&c2);
                                }
                            }
                            // 其余响应码（如530拒绝匿名）：握手完成，登录失败如实记录
                            _ => {}
                        }
                    }
                }
            }
        }
        result.elapsed_ms = start.elapsed().as_millis();
        (true, CheckResultDetail::Ftp(result))
    }

    fn get_type(&self) -> MonitorType {
        MonitorType::Ftp
    }
}

// 发送一条FTP命令（CRLF结尾）
async fn send_cmd(stream: &mut TcpStream, cmd: &str) -> std::io::Result<()> {
    stream
        .write_all(format!("{}\r\n", cmd).as_bytes())
        .await
}

// 读取一条完整的FTP回复（多行回复读至终结行），返回响应码；
// 读到的原始字节累积进buf供横幅展示。单次读超时3秒。
async fn read_ftp_reply(stream: &mut TcpStream, buf: &mut Vec<u8>) -> Result<u16, String> {
    let mut chunk = [0u8; 512];
    loop {
        if let Some(code) = parse_ftp_code(buf) {
            return Ok(code);
        }
        if buf.len() > 8 * 1024 {
            return Err("FTP回复过长".to_string());
        }
        match tokio::time::timeout(Duration::from_secs(3), stream.read(&mut chunk)).await {
            Ok(Ok(0)) => return Err("连接在对端关闭前未给出完整FTP回复".to_string()),
            Ok(Ok(n)) => buf.extend_from_slice(&chunk[..n]),
            Ok(Err(e)) => return Err(format!("读取FTP回复失败: {}", e)),
            Err(_) => return Err("等待FTP回复超时（3秒）".to_string()),
        }
    }
}

// 从缓冲中解析FTP回复码（RFC 959）：
// 首行"ddd"+空格/行尾 → 单行回复，直接返回码；
// 首行"ddd-" → 多行回复，直到出现"ddd"(同码)+空格/行尾的终结行才返回，
// 中间行可含任意内容（即使形如"123 noise"也不会误判）；
// 解析不到终结行返回None（调用方继续等待），非协议数据返回None。
fn parse_ftp_code(buf: &[u8]) -> Option<u16> {
    let text = String::from_utf8_lossy(buf);
    let mut lines = text.split("\r\n");
    // 首行决定回复码与单/多行形态
    let first = lines.next()?;
    let b = first.as_bytes();
    if b.len() < 3 || !b[..3].iter().all(u8::is_ascii_digit) {
        return None;
    }
    let code =
        (b[0] - b'0') as u16 * 100 + (b[1] - b'0') as u16 * 10 + (b[2] - b'0') as u16;
    if b.len() == 3 || b[3] == b' ' {
        return Some(code);
    }
    if b[3] != b'-' {
        return None; // "ddd"+其他字符：非协议内容
    }
    // 多行回复：终结行必须是同码 + 空格/行尾
    for line in lines {
        let lb = line.as_bytes();
        if lb.len() >= 3
            && lb[..3].iter().all(u8::is_ascii_digit)
            && lb[0] == b[0]
            && lb[1] == b[1]
            && lb[2] == b[2]
            && (lb.len() == 3 || lb[3] == b' ')
        {
            return Some(code);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{MonitorConfig, MonitorType, parse_ftp_code, FtpMonitor};
    use crate::monitor::types::CheckResultDetail;
    use crate::monitor::Monitor;

    #[test]
    fn ftp_reply_parser_handles_single_and_multiline() {
        assert_eq!(parse_ftp_code(b"220 ProFTPD Server ready\r\n"), Some(220));
        assert_eq!(
            parse_ftp_code(b"220-Welcome to FTP\r\nsome text\r\n220 Ready\r\n"),
            Some(220),
            "多行回复取终结行"
        );
        // 多行回复未到终结行：继续等待
        assert_eq!(parse_ftp_code(b"220-Welcome\r\n"), None);
        assert_eq!(parse_ftp_code(b""), None);
        // 任意TCP服务的垃圾数据不是合法FTP回复
        assert_eq!(parse_ftp_code(b"HTTP/1.1 200 OK\r\n\r\n"), None);
        // 4位数字开头不是响应码
        assert_eq!(parse_ftp_code(b"2200 garbage\r\n"), None);
        // 中间行混入类响应码内容不影响终结行判定
        assert_eq!(parse_ftp_code(b"331-x\r\n123 noise\r\n331 ok\r\n"), Some(331));
    }

    // 本地假FTP：按脚本回发响应，供端到端验证握手语义
    fn spawn_fake_ftp(script: &'static [&'static [u8]]) -> std::net::SocketAddr {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 256];
                for msg in script {
                    if !msg.is_empty() {
                        let _ = stream.write_all(msg);
                    }
                    let _ = stream.read(&mut buf); // 等客户端命令（横幅前的那次read会立即超时返回0，无碍）
                }
            }
        });
        addr
    }

    fn ftp_config(addr: std::net::SocketAddr) -> MonitorConfig {
        MonitorConfig {
            target: Some(format!("127.0.0.1:{}", addr.port())),
            interval: Some(60),
            monitor_type: MonitorType::Ftp,
            timeout: 5000,
            details: crate::monitor::types::MonitorConfigDetail::Ftp(
                crate::monitor::types::FtpMonitorConfig {},
            ),
        }
    }

    #[tokio::test]
    async fn ftp_monitor_completes_anonymous_handshake() {
        let addr = spawn_fake_ftp(&[
            b"220 fake-ftp ready\r\n",
            b"331 password required\r\n",
            b"230 login ok\r\n",
        ]);
        let (status, detail) = FtpMonitor::new().check(&ftp_config(addr)).await;
        assert!(status);
        let CheckResultDetail::Ftp(r) = detail else {
            panic!("应为Ftp结果");
        };
        assert!(r.connected && r.handshake_ok, "握手应完成: {r:?}");
        assert!(r.logged_in, "230应记为登录成功");
        assert_eq!(r.last_code, Some(230));
        assert!(r.banner.as_deref().unwrap().contains("fake-ftp"));
    }

    #[tokio::test]
    async fn ftp_monitor_reports_anonymous_refusal_as_handshake_only() {
        // 服务器讲FTP但拒绝匿名登录：握手完成（可用），logged_in=false
        let addr = spawn_fake_ftp(&[
            b"220 real ftp\r\n",
            b"530 anonymous login not allowed\r\n",
        ]);
        let (_status, detail) = FtpMonitor::new().check(&ftp_config(addr)).await;
        let CheckResultDetail::Ftp(r) = detail else {
            panic!("应为Ftp结果");
        };
        assert!(r.connected && r.handshake_ok, "协议握手应完成: {r:?}");
        assert!(!r.logged_in, "530应记为登录失败");
        assert_eq!(r.last_code, Some(530));
        assert!(r.available(), "服务在正常讲FTP，应判可用");
    }

    #[tokio::test]
    async fn ftp_monitor_rejects_non_ftp_service() {
        // 恰好开在21端口（此处任意端口）的非FTP服务：连接成功但握手失败
        let addr = spawn_fake_ftp(&[b"HTTP/1.1 200 OK\r\n\r\n"]);
        let (_status, detail) = FtpMonitor::new().check(&ftp_config(addr)).await;
        let CheckResultDetail::Ftp(r) = detail else {
            panic!("应为Ftp结果");
        };
        assert!(r.connected, "TCP连接本身成功");
        assert!(!r.handshake_ok, "非协议内容不应通过握手");
        assert!(!r.available(), "握手失败即不可用");
    }
}
