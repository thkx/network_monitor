use super::monitor_trait::Monitor;
use super::types::{
    CheckResultDetail, HttpMonitorConfig, MonitorConfig, MonitorConfigDetail,
    UnknownMonitorResult,
};
use crate::tools_types::{
    AdvancedAvailability, BasicAvailability, ContentVerificationResult,
    ContentVerificationRules, ContentVerificationRulesResult, ContentVerificationRulesSingle,
    HttpBody, HttpMethodTypes, HttpMonitorResult, MonitorType, PerformanceTimings, SecurityHeaders,
    StatusCategory, StatusInfo,
};
use regex::Regex;
use reqwest::Client;
use crate::tools::get_dns_tcp_tls_performance;
use std::collections::HashMap;
use std::time::{ Duration, Instant };
use uuid::Uuid;

pub struct HttpMonitor {
    client: Client,
}
impl HttpMonitor {
    pub fn new() -> Self {
        // 创建一个Client，方便后续进行URL调用
        HttpMonitor {
            client: Client::builder()
                .redirect(reqwest::redirect::Policy::limited(5)) // 重定向次数限制为5次
                .build()
                .expect("Failed to create HTTP client"),
        }
    }
    // 获取一个http 客户端链接 根据 method 方法来判断 创建一个http 链接
    pub fn get_client(&self, config: &HttpMonitorConfig) -> reqwest::RequestBuilder {
        //   创建一个http 请求
        let mut request_client = match config.method {
            HttpMethodTypes::Get => self.client.get(config.url.as_str()),
            HttpMethodTypes::Post => self.client.post(config.url.as_str()),
            HttpMethodTypes::Put => self.client.put(config.url.as_str()),
            HttpMethodTypes::Delete => self.client.delete(config.url.as_str()),
            HttpMethodTypes::Head => self.client.head(config.url.as_str()),
            HttpMethodTypes::Patch => self.client.patch(config.url.as_str()),
            _ => self.client.get(config.url.as_str()), // 默认使用 GET 方法
        };

        // 根据配置信息  设置超时时间
        request_client = request_client.timeout(Duration::from_millis(config.timeout));
        // 如果用户自定义了Header头信息 在这里要添加进去
        if let Some(headers) = &config.headers {
            request_client = request_client.headers(headers.clone());
        }
        // 兼容几种不同的body类型，分别进行设置
        if let Some(body) = &config.body {
            match body {
                HttpBody::Text(text) => {
                    request_client = request_client.body(text.clone());
                }
                HttpBody::Binary(bin) => {
                    request_client = request_client.body(bin.clone());
                }
                HttpBody::Json(json_value) => {
                    request_client = request_client.json(json_value);
                }
                HttpBody::Empty => {
                    request_client = request_client.body("");
                }
            }
        }
        // 最终返回一个HTTP连接 Client
        request_client
    }
    // 创建一个生成基础监控结果的函数
    // 解析成基础的响应结果结构体
    fn create_basic_result(
        &self,
        status_code: u16,
        version: reqwest::Version,
        headers: &reqwest::header::HeaderMap,
        content_length: Option<u64>,
    ) -> BasicAvailability {
        BasicAvailability {
            is_reachable: true,        // 代表这个target 是可以访问的
            dns_resolvable: true,      // 代表这个target 的域名是可以解析的
            tcp_connect_success: true, // 代表这个target 的 TCP 连接是成功的
            res_received: true,
            res_status_code: Some(status_code),
            res_status_category: StatusCategory::from_status_code(status_code), //获取对应的状态码描述信息
            protocol_version: format!("{:?}", version),                         // 获取HTTP协议版本
            res_content_type: headers
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string()),
            res_content_length: content_length.unwrap_or(0),
            res_charset: headers
                .get(reqwest::header::ACCEPT_CHARSET)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string()),
        }
    }

    // 创建一个生成安全头监控结果的函数
    // 解析相关头信息
    fn create_headers_result(&self, headers: &reqwest::header::HeaderMap) -> SecurityHeaders {
        SecurityHeaders {
            strict_transport_security: headers
                .get("Strict-Transport-Security")
                .as_ref()
                .map(|v| v.to_str().unwrap_or("").to_string()),
            content_security_policy: headers
                .get("Content-Security-Policy")
                .as_ref()
                .map(|v| v.to_str().unwrap_or("").to_string()),
            x_content_type_options: headers
                .get("X-Content-Type-Options")
                .as_ref()
                .map(|v| v.to_str().unwrap_or("").to_string()),
            x_frame_options: headers
                .get("X-Frame-Options")
                .as_ref()
                .map(|v| v.to_str().unwrap_or("").to_string()),
            x_xss_protection: headers
                .get("X-XSS-Protection")
                .as_ref()
                .map(|v| v.to_str().unwrap_or("").to_string()),
            referrer_policy: headers
                .get("Referrer-Policy")
                .as_ref()
                .map(|v| v.to_str().unwrap_or("").to_string()),
            feature_policy: headers
                .get("Feature-Policy")
                .as_ref()
                .map(|v| v.to_str().unwrap_or("").to_string()),
            permissions_policy: headers
                .get("Permissions-Policy")
                .as_ref()
                .map(|v| v.to_str().unwrap_or("").to_string()),
            security_headers_ok: self.check_security_headers_ok(headers),
        }
    }
    //  检测是否安全头有效
    fn check_security_headers_ok(&self, headers: &reqwest::header::HeaderMap) -> bool {
        let mut score = 0;
        if headers.get("Strict-Transport-Security").is_some() {
            score += 1;
        }
        if headers.get("Content-Security-Policy").is_some() {
            score += 1;
        }
        if headers.get("X-Content-Type-Options").is_some() {
            score += 1;
        }
        if headers.get("X-Frame-Options").is_some() {
            score += 1;
        }
        if headers.get("X-XSS-Protection").is_some() {
            score += 1;
        }
        if headers.get("Referrer-Policy").is_some() {
            score += 1;
        }
        if headers.get("Permissions-Policy").is_some() {
            score += 1;
        }
        score >= 4 // 例如，至少有4个安全头部存在则认为安全头部检查通过
    }

    // 创建一个校验响应内容监控结果的函数
    // 创建内容验证结果
    fn create_content_verification_result(
        &self,
        body: &str,
        rules: &Option<Vec<ContentVerificationRulesSingle>>,
    ) -> ContentVerificationResult {
        // 判空处理
        if rules.is_none() || body.is_empty() {
            return ContentVerificationResult {
                match_rules: vec![],
                failed_rules: vec![],
            };
        }
        let rules = rules.as_ref().unwrap();
        let mut match_rules = vec![];
        let mut failed_rules = vec![];

        for ContentVerificationRulesSingle {
            rule_type,
            rule_content,
            rule_description,
        } in rules.iter()
        {
            let status = match rule_type {
                // 是否包含哪些内容？
                ContentVerificationRules::Contains => body.contains(rule_content),
                // 是否不包含哪些内容？
                ContentVerificationRules::NotContains => !body.contains(rule_content),
                // 匹配正则表达式（非法正则视为未命中，不 panic；API 层已预校验，这里兜底）
                ContentVerificationRules::Regex => match Regex::new(rule_content) {
                    Ok(regex) => regex.is_match(body),
                    Err(_) => false,
                }
                _ => false,
            };
            self.get_content_verify_result(
                status,
                rule_type.clone(),
                rule_content.clone(),
                rule_description.clone(),
                &mut match_rules,
                &mut failed_rules,
            );
        }
        ContentVerificationResult {
            match_rules,
            failed_rules,
        }
    }
    // 拼接内容校验的内容 因为不管是什么类型 返回的数据结构都是一样的
    fn get_content_verify_result(
        &self,
        status: bool,
        rule_type: ContentVerificationRules,
        rule_content: String,
        rule_description: String,
        true_rules: &mut Vec<ContentVerificationRulesResult>,
        failed_rules: &mut Vec<ContentVerificationRulesResult>,
    ) {
        let result = ContentVerificationRulesResult {
            rule_id: Uuid::new_v4().as_u128() as u64,
            status: if status {
                StatusInfo::Success
            } else {
                StatusInfo::Failed
            },
            message: if status {
                "Rule verification passed".to_string()
            } else {
                "Rule verification failed".to_string()
            },
            rules: ContentVerificationRulesSingle {
                rule_type: rule_type.clone(),
                rule_content: rule_content.clone(),
                rule_description: rule_description.clone(),
            },
        };
        if status {
            true_rules.push(result);
        } else {
            failed_rules.push(result);
        }
    }

    // 业务指标提取：响应体为JSON时按配置的点路径提取值（如 "code"、"data.queue"）。
    // 提取失败（非JSON/路径不存在）静默跳过——业务指标是增强信息，不影响可用性判定
    fn create_advanced_availability_result(
        &self,
        body: &str,
        fields: &[String],
    ) -> AdvancedAvailability {
        if fields.is_empty() {
            return AdvancedAvailability::default();
        }
        let Ok(json) = serde_json::from_str::<serde_json::Value>(body) else {
            return AdvancedAvailability::default();
        };
        let mut business_metrics = HashMap::new();
        for path in fields {
            if let Some(value) = extract_json_path(&json, path) {
                business_metrics.insert(path.clone(), value);
            }
        }
        AdvancedAvailability { business_metrics }
    }
}

// 按点路径提取JSON值：对象键逐层下钻，数组段用数字下标（如 "items.0.id"）。
// 字符串返回原文，数字/布尔等返回JSON文本表示；路径不存在返回None
fn extract_json_path(json: &serde_json::Value, path: &str) -> Option<String> {
    let mut current = json;
    for segment in path.split('.') {
        current = if current.is_array() {
            current.get(segment.parse::<usize>().ok()?)?
        } else {
            current.get(segment)?
        };
    }
    match current {
        serde_json::Value::String(s) => Some(s.clone()),
        other => Some(other.to_string()),
    }
}
//  下面的话 就是要实现具体的check方法了
#[async_trait::async_trait]
impl Monitor for HttpMonitor {
    async fn check(&self, config: &MonitorConfig) -> (bool, CheckResultDetail) {
        // 这里就是我们的主战场了 首先我们先把异常情况写完，然后在后面进行一点点的填空
        // 判断一下是否存在 target 此时的target 应该是一个 URL 地址
        if config.target.is_none() {
            return (
                false, // 监控状态失败 根本就没有进入监控当中去
                CheckResultDetail::Http(HttpMonitorResult::default()),
            );
        }
        // 来处理具体的HTTP监控的类型数据
        match config.details {
            MonitorConfigDetail::Http(ref detail) => {
                // 在这里的话 我们在前一章里面也看到了 首先我们要发送HTTP请求到Target地址
                let request_client = self.get_client(detail);
                // 记录请求开始时间，用于统计总耗时等性能指标
                let start_time_instant = Instant::now();
                // 发送请求，来匹配返回的结果
                match request_client.send().await {
                    Ok(response) => {
                        // send() 返回即已收到响应头，此前耗时近似首字节时间（TTFB）
                        let ttfb_ms = start_time_instant.elapsed().as_millis();
                        // 获取状态码（探测连接失败时各项为0、证书为None，正好是tuple的Default值）
                        let (
                            dns_lookup_time,
                            tcp_connect_time,
                            tls_handshake_time,
                            ssl_certificate_info,
                        ) = get_dns_tcp_tls_performance(config.target.as_ref().unwrap())
                            .await
                            .unwrap_or_default();
                        // 提取需要的数据
                        let status_code = response.status().as_u16();
                        let version = response.version();
                        let headers = response.headers().clone();
                        let content_length = response.content_length();
                        // 因为这里的代码会获取所有权 所以后面就没办法直接用response了 所以上面就单独获取response中的字段值用在后面的方法中
                        let body = response.text().await.unwrap_or_default();
                        let total_time = start_time_instant.elapsed().as_millis();
                        // 创建基本结果和头部结果
                        let basic_available = self.create_basic_result(
                            status_code,
                            version,
                            &headers,
                            content_length,
                        );
                        let response_headers = self.create_headers_result(&headers);
                        // 客户端可实测的指标：TTFB（send返回）、内容下载（total-ttfb）、总耗时
                        // dns/tcp/tls 来自另建探测连接的近似值；服务端处理耗时无法从客户端测得，保持0
                        let performance_timings = PerformanceTimings {
                            dns_lookup_time,
                            tcp_connect_time,
                            tls_handshake_time,
                            first_byte_time: ttfb_ms,
                            content_download_time: total_time.saturating_sub(ttfb_ms),
                            response_receiving_time: total_time.saturating_sub(ttfb_ms),
                            total_time,
                            ssl_cert_valid: ssl_certificate_info
                                .as_ref()
                                .map(|c| c.is_valid)
                                .unwrap_or(false),
                            ..Default::default()
                        };
                        return (
                            true, // 监控任务执行成功；目标是否可用看 basic_available.is_reachable
                            CheckResultDetail::Http(HttpMonitorResult {
                                basic_available,
                                response_headers,
                                performance_timings,
                                certificate_info: ssl_certificate_info.unwrap_or_default(),
                                content_verification: self
                                    .create_content_verification_result(&body, &detail.rules),
                                advanced_available: self.create_advanced_availability_result(
                                    &body,
                                    &detail.business_metric_fields,
                                ),
                                error_message: None,
                                error_kind: None,
                            }),
                        );
                    }
                    Err(e) => {
                        // 保留失败原因与错误类别：此前直接丢弃 Err(e)，日志只有 Failed 无法定位原因
                        let kind = if e.is_timeout() {
                            "timeout"
                        } else if e.is_connect() {
                            "connect"
                        } else if e.is_decode() {
                            "decode"
                        } else {
                            "other"
                        };
                        return (
                            true, // 监控任务执行成功；目标不可达体现在 is_reachable=false
                            CheckResultDetail::Http(HttpMonitorResult {
                                basic_available: BasicAvailability {
                                    is_reachable: false,
                                    ..Default::default()
                                },
                                error_message: Some(e.to_string()),
                                error_kind: Some(kind.to_string()),
                                ..Default::default()
                            }),
                        );
                    }
                }
            }
            _ => {
                // 如果不是 HTTP 监控类型，返回错误结果
                return (
                    false, // 监控状态失败 根本就没有进入监控当中去
                    CheckResultDetail::Unknown(UnknownMonitorResult {
                        description: "调用监控类型: HTTP, 请查实后再继续操作".to_string(),
                        query_type: MonitorType::Http,
                    }),
                );
            }
        }
    }
    // 定义返回具体类型的函数
    fn get_type(&self) -> MonitorType {
        MonitorType::Http
    }
}



#[cfg(test)]
mod tests {
    use super::{extract_json_path, HttpMonitor};
    use crate::monitor::Monitor;
    use crate::monitor::types::{
        CheckResultDetail, HttpMonitorConfig, MonitorConfig, MonitorConfigDetail,
    };
    use crate::tools_types::{
        ContentVerificationRules, ContentVerificationRulesSingle, HttpMethodTypes, MonitorType,
    };

    // 本地假HTTP服务器：读完整请求头后再回响应（一次read只可能拿到分段报文的碎片，
    // 带着未读请求字节关连接会触发RST把客户端的body读取重置——webhook假服务器的同款教训）
    fn spawn_fake_http(response: String) -> std::net::SocketAddr {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = Vec::with_capacity(1024);
                let mut chunk = [0u8; 512];
                loop {
                    match stream.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            buf.extend_from_slice(&chunk[..n]);
                            if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                                break;
                            }
                        }
                    }
                }
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });
        addr
    }

    fn canned_200(content_type: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn http_config(
        addr: std::net::SocketAddr,
        rules: Vec<ContentVerificationRulesSingle>,
        metric_fields: Vec<String>,
    ) -> MonitorConfig {
        MonitorConfig {
            target: Some(format!("http://{}", addr)),
            interval: Some(60),
            monitor_type: MonitorType::Http,
            // 8秒：send()之后、body读取之前还有一次DNS/TCP/TLS探测连接（近似值设计），
            // resolver首次初始化可达秒级，需给body读取留足请求总超时预算
            timeout: 8000,
            details: MonitorConfigDetail::Http(HttpMonitorConfig {
                url: format!("http://{}", addr),
                method: HttpMethodTypes::Get,
                timeout: 8000,
                headers: None,
                body: None,
                rules: if rules.is_empty() { None } else { Some(rules) },
                business_metric_fields: metric_fields,
            }),
        }
    }

    fn rule(kind: ContentVerificationRules, content: &str) -> ContentVerificationRulesSingle {
        ContentVerificationRulesSingle {
            rule_type: kind,
            rule_content: content.to_string(),
            rule_description: String::new(),
        }
    }

    // 可达200 + 内容规则命中：基础可用性与校验结果同时成立
    #[tokio::test]
    async fn reachable_200_with_matching_content_rule() {
        let addr = spawn_fake_http(canned_200("text/plain", "hello world"));
        let (status, detail) = HttpMonitor::new()
            .check(&http_config(addr, vec![rule(ContentVerificationRules::Contains, "hello")], vec![]))
            .await;
        assert!(status);
        let CheckResultDetail::Http(r) = detail else {
            panic!("应为HTTP结果");
        };
        assert!(r.basic_available.is_reachable);
        assert_eq!(r.basic_available.res_status_code, Some(200));
        assert_eq!(r.content_verification.match_rules.len(), 1);
        assert!(r.content_verification.failed_rules.is_empty());
    }

    // 内容规则未命中：目标仍可达（内容校验不影响可用性），失败项如实记录
    #[tokio::test]
    async fn failed_content_rules_are_recorded() {
        let addr = spawn_fake_http(canned_200("text/plain", "hello world"));
        let (_, detail) = HttpMonitor::new()
            .check(&http_config(addr, vec![rule(ContentVerificationRules::NotContains, "hello")], vec![]))
            .await;
        let CheckResultDetail::Http(r) = detail else {
            panic!("应为HTTP结果");
        };
        assert!(r.basic_available.is_reachable);
        assert_eq!(r.content_verification.failed_rules.len(), 1);
    }

    // 业务指标提取：JSON响应体按点路径提取，路径缺失静默跳过
    #[tokio::test]
    async fn business_metrics_extracted_from_json_body() {
        let body = r#"{"code":0,"data":{"queue":42}}"#;
        let addr = spawn_fake_http(canned_200("application/json", body));
        let (_, detail) = HttpMonitor::new()
            .check(&http_config(
                addr,
                vec![],
                vec!["code".into(), "data.queue".into(), "missing.path".into()],
            ))
            .await;
        let CheckResultDetail::Http(r) = detail else {
            panic!("应为HTTP结果");
        };
        assert_eq!(
            r.advanced_available.business_metrics.get("code").map(String::as_str),
            Some("0")
        );
        assert_eq!(
            r.advanced_available.business_metrics.get("data.queue").map(String::as_str),
            Some("42")
        );
        assert!(!r.advanced_available.business_metrics.contains_key("missing.path"));
    }

    // 连接被拒：任务执行成功但目标不可达，错误分类为connect并携带原因
    #[tokio::test]
    async fn connection_refused_marks_unreachable() {
        // 127.0.0.1:9（discard）无监听必然拒绝；不用"绑定后释放"取端口——
        // 并行测试里其他假服务器可能恰好复用该端口造成偶发
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], 9));
        let (status, detail) = HttpMonitor::new().check(&http_config(addr, vec![], vec![])).await;
        assert!(status, "任务本身执行成功，不可达体现在结果里");
        let CheckResultDetail::Http(r) = detail else {
            panic!("应为HTTP结果");
        };
        assert!(!r.basic_available.is_reachable);
        // 端口9的失败方式随环境而异：真拒绝为connect，被防火墙DROP则表现为timeout
        assert!(
            matches!(r.error_kind.as_deref(), Some("connect" | "timeout")),
            "错误类别应为connect或timeout: {:?}",
            r.error_kind
        );
        assert!(r.error_message.is_some());
    }

    // 点路径提取：嵌套对象、数组下标、标量字符串化、缺失路径
    #[test]
    fn json_path_extracts_nested_and_array_values() {
        let json: serde_json::Value =
            serde_json::from_str(r#"{"a":{"b":[10,{"c":"x"}]},"n":5,"s":"v"}"#).unwrap();
        assert_eq!(extract_json_path(&json, "n").as_deref(), Some("5"));
        assert_eq!(extract_json_path(&json, "s").as_deref(), Some("v"));
        assert_eq!(extract_json_path(&json, "a.b.0").as_deref(), Some("10"));
        assert_eq!(extract_json_path(&json, "a.b.1.c").as_deref(), Some("x"));
        assert_eq!(extract_json_path(&json, "a.b.9"), None);
        assert_eq!(extract_json_path(&json, "nope"), None);
    }
}
