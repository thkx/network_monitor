use super::Monitor;
use super::types::{CheckResultDetail, IcmpMonitorResult, MonitorConfig};
use crate::domain::MonitorType;
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::process::Command;

pub struct IcmpMonitor {}

impl IcmpMonitor {
    pub fn new() -> Self {
        IcmpMonitor {}
    }
}

// 从 ping 命令输出中解析单包 ICMP 往返时延（毫秒）。
// 覆盖各平台/locale 的常见形态：
//   Linux/macOS 英文: "time=1.23 ms" / "time=0.456ms"
//   Windows 英文:     "time=1ms" / "time<1ms"
//   Windows 中文:     "时间=1ms" / "时间<1ms" / "时间=1毫秒"（部分系统）
//   Linux 极小值:     "time<0.1 ms"
// 键取 time / 时间，符号取 = 或 <（<1ms 记为该上界值，足够作监控参考）。
// 解析不到返回 None——调用方回退到墙钟总耗时，最坏不劣于旧行为。
// 只取第一个匹配：单包探测输出里至多一条时延行
fn parse_rtt_ms(output: &str) -> Option<f64> {
    // 逐字节扫描，遇到 "time" 或 "时间" 键后跳过 =/< 与空白，读取其后的数字
    // （不引正则依赖：模式足够简单，手写扫描更可控且零成本）
    const KEYS: [&str; 2] = ["time", "时间"];
    for key in KEYS {
        let mut search_from = 0;
        while let Some(pos) = output[search_from..].find(key) {
            let after_key = search_from + pos + key.len();
            search_from = after_key; // 下一轮从本次键之后继续找，避免死循环
            let rest = &output[after_key..];
            // 键后应紧跟 = 或 <（允许其间有空白），否则不是时延字段（如 "timestamp"）
            let mut chars = rest.char_indices().skip_while(|(_, c)| c.is_whitespace());
            let Some((sep_idx, sep)) = chars.next() else {
                continue;
            };
            if sep != '=' && sep != '<' {
                continue;
            }
            // 符号之后跳过空白，累积数字（含小数点）
            let num_start = after_key + sep_idx + sep.len_utf8();
            let num: String = output[num_start..]
                .chars()
                .skip_while(|c| c.is_whitespace())
                .take_while(|c| c.is_ascii_digit() || *c == '.')
                .collect();
            if let Ok(v) = num.parse::<f64>()
                && v >= 0.0
            {
                return Some(v);
            }
        }
    }
    None
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
            Ok(child) => {
                tokio::time::timeout(Duration::from_millis(timeout_ms), child.wait_with_output())
                    .await
                    .ok()
                    .and_then(|r| r.ok())
            }
            Err(_) => None, // ping命令不存在等启动失败：按不可用处理
        };
        let elapsed_ms = start.elapsed().as_millis();
        // ping命令执行成功（退出码为0）即代表目标主机存活；超时/启动失败均不可用
        let is_alive = matches!(&output, Some(o) if o.status.code() == Some(0));
        // 存活时从 stdout 解析真实链路 RTT；解析失败为 None，落库时回退墙钟总耗时。
        // 不存活不解析（无有效时延行）
        let rtt_ms = if is_alive {
            output
                .as_ref()
                .and_then(|o| parse_rtt_ms(&String::from_utf8_lossy(&o.stdout)))
        } else {
            None
        };
        (
            true,
            CheckResultDetail::Icmp(IcmpMonitorResult {
                is_alive,
                elapsed_ms,
                rtt_ms,
            }),
        )
    }

    fn get_type(&self) -> MonitorType {
        MonitorType::Icmp
    }
}

#[cfg(test)]
mod tests {
    use super::parse_rtt_ms;

    #[test]
    fn parses_linux_output_with_and_without_space_before_ms() {
        // "time=12.3 ms"（有空格）与 "time=0.456ms"（无空格）走同一解析路径：
        // take_while 在数字/小数点后即停，ms 前的空格与否不影响结果
        let with_space = "PING host (1.2.3.4) 56(84) bytes of data.\n\
                          64 bytes from 1.2.3.4: icmp_seq=1 ttl=55 time=12.3 ms\n";
        assert_eq!(parse_rtt_ms(with_space), Some(12.3));
        let no_space = "64 bytes from 1.1.1.1: icmp_seq=1 ttl=64 time=0.456ms\n";
        assert_eq!(parse_rtt_ms(no_space), Some(0.456));
    }

    #[test]
    fn parses_windows_english_output() {
        let out = "Reply from 1.2.3.4: bytes=32 time=1ms TTL=117\n";
        assert_eq!(parse_rtt_ms(out), Some(1.0));
    }

    #[test]
    fn parses_windows_less_than_one_ms() {
        // "time<1ms"：记为上界值1，足够作监控参考
        let out = "Reply from 127.0.0.1: bytes=32 time<1ms TTL=128\n";
        assert_eq!(parse_rtt_ms(out), Some(1.0));
    }

    #[test]
    fn parses_windows_chinese_output() {
        let out = "来自 1.2.3.4 的回复: 字节=32 时间=5ms TTL=117\n";
        assert_eq!(parse_rtt_ms(out), Some(5.0));
    }

    #[test]
    fn returns_none_when_no_time_field() {
        // 超时/无回包：无时延字段
        let out = "Request timed out.\n请求超时。\n";
        assert_eq!(parse_rtt_ms(out), None);
    }

    #[test]
    fn ignores_timestamp_like_keys() {
        // "timestamp=..." 的 time 前缀不应被误当时延（键后非 =/< 紧邻数字的时延语义）
        // 这里构造一个只含 timestamp 无真实 time= 的串，应返回 None
        let out = "log timestamp 2024 no rtt here\n";
        assert_eq!(parse_rtt_ms(out), None);
    }

    #[test]
    fn takes_first_match_on_multiline() {
        let out = "64 bytes from x: icmp_seq=1 ttl=55 time=9.9 ms\n\
                   64 bytes from x: icmp_seq=2 ttl=55 time=8.8 ms\n";
        assert_eq!(parse_rtt_ms(out), Some(9.9), "取第一个匹配");
    }
}
