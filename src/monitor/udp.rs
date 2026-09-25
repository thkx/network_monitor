use super::Monitor;
use super::types::{CheckResultDetail, MonitorConfig, UdpMonitorResult};
use crate::domain::MonitorType;
use crate::tools::parse_host_port;
use std::time::Instant;
use tokio::net::UdpSocket;

pub struct UdpMonitor {}

impl UdpMonitor {
    pub fn new() -> Self {
        UdpMonitor {}
    }
}

// UDP探测的诚实边界：
// - 端口53（DNS）按DNS协议语义探测：构造真实A查询，应答须匹配事务ID且QR=1，
//   这是真实的协议交互，健康检查结果可信；
// - 其余UDP端口没有通用的"ping"协议，只能发送载荷并等待任意回包——
//   静默丢弃未知载荷的服务（大多数UDP服务如此）会被判无响应，
//   因此UDP监控仅对有应答语义的服务（回显、自定义协议等）有意义，配置时需知悉。
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
        // 端口53走DNS语义；host为域名时查询该域名自身，是IP时查询localhost
        let dns_mode = port == 53;
        let qname = if host.parse::<std::net::IpAddr>().is_err() {
            host.as_str()
        } else {
            "localhost"
        };
        let start = Instant::now();
        let mut sent = false; // 是否成功发送探测包
        let mut response_received = false; // 是否收到（有效的）响应
        // 先把目标地址解析为 SocketAddr
        if let Ok(mut addrs) = tokio::net::lookup_host((host.as_str(), port)).await
            && let Some(addr) = addrs.next()
        {
            // 绑定一个随机本地端口用于发送和接收
            // 等待响应超时对齐config.timeout（毫秒；下限1秒防配置0导致立即超时，与ICMP同规则）
            let wait = std::time::Duration::from_millis(config.timeout.max(1000));
            if let Ok(socket) = UdpSocket::bind("0.0.0.0:0").await {
                // DNS模式发送真实查询报文；其余端口发送简单载荷等任意回包
                let txid = uuid::Uuid::new_v4().as_u128() as u16;
                let payload = if dns_mode {
                    build_dns_query(txid, qname)
                } else {
                    b"ping".to_vec()
                };
                if socket.send_to(&payload, addr).await.is_ok() {
                    sent = true;
                    let mut buf = [0u8; 1024];
                    // 等待响应，超时时间由config.timeout决定（见上方wait）
                    if let Ok(Ok((n, src))) =
                        tokio::time::timeout(wait, socket.recv_from(&mut buf)).await
                    {
                        // 源地址校验：UDP无连接，recv_from会收到任意来源的包（局域网广播、
                        // 无关服务、反射流量都可能污染结果）。仅接受来自探测目标的回包，
                        // 排除"隔壁主机的包让健康检查误判为可用"
                        let from_target = src == addr;
                        // DNS模式下再校验：事务ID匹配 + QR位=1（是应答而非查询），
                        // 排除端口上无关服务/反射干扰的噪声报文
                        response_received = from_target
                            && if dns_mode {
                                is_valid_dns_reply(&buf[..n], txid)
                            } else {
                                true
                            };
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
                dns_mode,
            }),
        )
    }

    fn get_type(&self) -> MonitorType {
        MonitorType::Udp
    }
}

// 构造最小DNS查询报文（RFC 1035）：随机事务ID + 标准查询标志（RD=1）+ 单条A/IN问题
fn build_dns_query(txid: u16, qname: &str) -> Vec<u8> {
    let mut pkt = Vec::with_capacity(12 + qname.len() + 6);
    pkt.extend_from_slice(&txid.to_be_bytes());
    pkt.extend_from_slice(&[0x01, 0x00]); // flags: 标准查询，RD=1
    pkt.extend_from_slice(&[0, 1, 0, 0, 0, 0, 0, 0]); // QDCOUNT=1，其余0
    for label in qname.split('.').filter(|s| !s.is_empty()) {
        pkt.push(label.len() as u8);
        pkt.extend_from_slice(label.as_bytes());
    }
    pkt.push(0); // 根标签结束
    pkt.extend_from_slice(&[0, 1]); // QTYPE=A
    pkt.extend_from_slice(&[0, 1]); // QCLASS=IN
    pkt
}

// 校验DNS应答：长度合规、事务ID匹配、QR位=1
fn is_valid_dns_reply(buf: &[u8], txid: u16) -> bool {
    buf.len() >= 12 && buf[0..2] == txid.to_be_bytes() && buf[2] & 0x80 != 0
}

#[cfg(test)]
mod tests {
    use super::{build_dns_query, is_valid_dns_reply};

    #[test]
    fn dns_query_packet_shape() {
        let pkt = build_dns_query(0xABCD, "www.example.com");
        assert_eq!(&pkt[0..2], &[0xAB, 0xCD], "事务ID大端在前");
        assert_eq!(&pkt[2..4], &[0x01, 0x00], "标准查询RD=1");
        assert_eq!(&pkt[4..6], &[0, 1], "问题数=1");
        // QNAME：3www 7example 3com 0
        assert_eq!(
            &pkt[12..pkt.len() - 4],
            &[
                3, b'w', b'w', b'w', 7, b'e', b'x', b'a', b'm', b'p', b'l', b'e', 3, b'c', b'o',
                b'm', 0
            ]
        );
        assert_eq!(&pkt[pkt.len() - 4..], &[0, 1, 0, 1], "QTYPE=A QCLASS=IN");
    }

    #[test]
    fn dns_query_single_label_name() {
        let pkt = build_dns_query(1, "localhost");
        // QNAME = 长度字节(1) + localhost(9) + 根结束(1) = 11字节，总长12+11+4=27
        assert_eq!(pkt.len(), 27);
    }

    #[test]
    fn dns_reply_validation() {
        let mut reply = vec![0xAB, 0xCD, 0x81, 0x80, 0, 1, 0, 1, 0, 0, 0, 0]; // QR=1
        assert!(is_valid_dns_reply(&reply, 0xABCD));
        reply[0] = 0x00; // 事务ID不匹配
        assert!(!is_valid_dns_reply(&reply, 0xABCD));
        reply[0] = 0xAB;
        reply[2] = 0x01; // QR=0：是查询不是应答
        assert!(!is_valid_dns_reply(&reply, 0xABCD));
        assert!(!is_valid_dns_reply(&reply[..8], 0xABCD), "过短报文拒绝");
    }

    // 源地址校验的happy path：本机回显服务器从探测目标地址回包，应判为已响应。
    // （mismatch路径需伪造源地址，UDP层无法在集成测试中稳定构造，此处覆盖正向路径）
    #[tokio::test]
    async fn udp_response_from_target_is_accepted() {
        use super::UdpMonitor;
        use crate::domain::MonitorType;
        use crate::monitor::Monitor;
        use crate::monitor::types::{
            CheckResultDetail, MonitorConfig, MonitorConfigDetail, UdpMonitorConfig,
        };

        // 回显服务器：收到任意包原样回发（非DNS端口，走"任意回包"语义）
        let server = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let server_addr = server.local_addr().unwrap();
        tokio::spawn(async move {
            let mut buf = [0u8; 1024];
            if let Ok((n, src)) = server.recv_from(&mut buf).await {
                let _ = server.send_to(&buf[..n], src).await;
            }
        });

        let config = MonitorConfig {
            target: Some(format!("127.0.0.1:{}", server_addr.port())),
            interval: Some(60),
            monitor_type: MonitorType::Udp,
            timeout: 1500,
            details: MonitorConfigDetail::Udp(UdpMonitorConfig {}),
        };
        let (status, detail) = UdpMonitor::new().check(&config).await;
        assert!(status);
        let CheckResultDetail::Udp(r) = detail else {
            panic!("应为UDP结果");
        };
        assert!(r.sent, "探测包应发送成功");
        assert!(r.response_received, "来自目标地址的回包应被接受");
        assert!(!r.dns_mode, "非53端口不走DNS语义");
    }
}
