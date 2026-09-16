use crate::tools_types::CertificateInfo;
use std::io;
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio::time::timeout;
// TLS相关：使用native-tls建立TLS连接，tokio-native-tls提供异步封装
use native_tls::TlsConnector;
use tokio_native_tls::TlsConnector as TokioTlsConnector;
use x509_parser::prelude::*;


// 单独定义一个获取TCP、DNS、TLS的函数

//定义一个函数返回类型为一个元组即可
type DnsTcpTlsPerformance = (u128, u128, u128, Option<CertificateInfo>); // DNS时间，TCP时间，TLS时间，证书信息

// 计算 性能监控相关数据 DNS TCP TLS 三个数据耗时 以及在连接过程中SSL证书相关信息
pub async fn get_dns_tcp_tls_performance(
    url: &str,
) -> Result<DnsTcpTlsPerformance, Box<dyn std::error::Error>> {
    // 使用Url进行解析相关的地址
    let url = reqwest::Url::parse(url)?;
    let host = url.host_str().ok_or("Invalid URL")?;
    let port = url.port_or_known_default().unwrap_or(80);
    let resolver = trust_dns_resolver::TokioAsyncResolver::tokio_from_system_conf()?;
    let dns_lookup_time = std::time::Instant::now();
    let ips = resolver.lookup_ip(host).await?;
    // 开始设置DNS的缓存时间
    let dns_lookup_ms = dns_lookup_time.elapsed().as_millis();
    // 开始设置TCP的缓存时间以及 TLS的时间
    // 遍历相关的IP 地址 取其中最小的一个时间即可
    let mut tls_min_time: Option<u128> = None;
    let mut tcp_min_time: Option<u128> = None;

    let per_addr_timeout = Duration::from_secs(3);
    let is_https = url.scheme() == "https";
    // 复用 TLS 连接器
    let tls_connector = if is_https {
        Some(TokioTlsConnector::from(TlsConnector::new()?))
    } else {
        None
    };
    let mut ssl_certificate_info: Option<crate::tools_types::CertificateInfo> = None;
    for ip in ips.iter() {
        let addr = std::net::SocketAddr::new(ip, port);
        let start_tcp = std::time::Instant::now();
        // 设置一个超时时间，避免个别IP无法连接阻塞后续的连接
        let tcp_res = tokio::time::timeout(per_addr_timeout, TcpStream::connect(addr)).await;
        let tcp_stream = match tcp_res {
            Ok(Ok(stream)) => {
                let ms = start_tcp.elapsed().as_millis();
                // 设置TCP链接时间
                tcp_min_time = tcp_min_time.map_or(Some(ms), |min| Some(min.min(ms)));
                stream
            }
            _ => continue, // 超时或连接失败，尝试下一个地址
        };
        if let Some(tls) = &tls_connector {
            let hs_start = Instant::now();
            let hs_res = timeout(per_addr_timeout, tls.connect(host, tcp_stream)).await;
            if let Ok(Ok(_tls_stream)) = hs_res {
                let ms = hs_start.elapsed().as_millis();
                // 设置TLS的连接时间
                tls_min_time = tls_min_time.map_or(Some(ms), |min| Some(min.min(ms)));
                // 检测证书有效性可以在这里进行
                if ssl_certificate_info.is_none()
                    && let Some(cert) = _tls_stream.get_ref().peer_certificate()? {
                        let cert_der = cert.to_der()?;
                        // from_der 返回 nom::IResult，需要手动 map_err
                        let (_, x509_cert) = X509Certificate::from_der(&cert_der).map_err(|e| {
                            io::Error::new(
                                io::ErrorKind::InvalidData,
                                format!("x509 parse error: {e}"),
                            )
                        })?;

                        let validity = x509_cert.validity();
                        let now = std::time::SystemTime::now();
                        let not_before = validity.not_before.to_datetime();
                        let not_after = validity.not_after.to_datetime();
                        let days_until_expiry = (not_after - now).whole_days();
                        // 这里的话需要查看相关的API文档来获取相关的库的信息
                        let info = CertificateInfo {
                            issuer: Some(x509_cert.issuer().to_string()),
                            subject: Some(x509_cert.subject().to_string()),
                            valid_from: Some(not_before.to_string()),
                            valid_until: Some(not_after.to_string()),
                            serial_number: Some(x509_cert.tbs_certificate.serial.to_string()),
                            signature_algorithm: Some(
                                x509_cert.signature_algorithm.algorithm.to_string(),
                            ),
                            public_key_algorithm: Some(format!(
                                "{:?}",
                                x509_cert.public_key().algorithm
                            )),
                            public_key_size: Some(
                                x509_cert.public_key().subject_public_key.data.len() * 8,
                            ), // 转换为比特
                            is_valid: days_until_expiry > 0,
                        };
                        ssl_certificate_info = Some(info);
                    }
            }
        }
    }
    let tcp = tcp_min_time.ok_or("all TCP connect attempts failed")?;
    let tls = if is_https {
        tls_min_time.unwrap_or(0)
    } else {
        0
    };
    // 返回相关结果
    Ok((dns_lookup_ms, tcp, tls, ssl_certificate_info))
}

/// 解析 host:port 格式的目标地址（兼容 http://host:port/ 这类带协议前缀的写法）
/// 未指定端口时使用 default_port
pub fn parse_host_port(target: &str, default_port: u16) -> Option<(String, u16)> {
    let t = target
        .trim()
        .trim_start_matches("http://")
        .trim_start_matches("https://");
    let authority = t.split('/').next()?;
    match authority.rsplit_once(':') {
        Some((host, port)) => Some((host.to_string(), port.parse().ok()?)),
        None => Some((authority.to_string(), default_port)),
    }
}

#[cfg(test)]
mod tests {
    use super::parse_host_port;

    #[test]
    fn bare_host_uses_default_port() {
        assert_eq!(
            parse_host_port("example.com", 80),
            Some(("example.com".to_string(), 80))
        );
    }

    #[test]
    fn host_with_port_is_parsed() {
        assert_eq!(
            parse_host_port("example.com:8443", 80),
            Some(("example.com".to_string(), 8443))
        );
    }

    #[test]
    fn protocol_prefix_and_path_are_stripped() {
        assert_eq!(
            parse_host_port("https://example.com:8443/path/to", 21),
            Some(("example.com".to_string(), 8443))
        );
        assert_eq!(
            parse_host_port("http://example.com", 8080),
            Some(("example.com".to_string(), 8080))
        );
    }

    #[test]
    fn ipv6_with_port_is_parsed() {
        assert_eq!(
            parse_host_port("[::1]:53", 53),
            Some(("[::1]".to_string(), 53))
        );
    }

    #[test]
    fn invalid_port_returns_none() {
        assert_eq!(parse_host_port("example.com:not_a_port", 80), None);
    }

    #[test]
    fn surrounding_whitespace_is_trimmed() {
        assert_eq!(
            parse_host_port("  example.com:22  ", 22),
            Some(("example.com".to_string(), 22))
        );
    }
}
