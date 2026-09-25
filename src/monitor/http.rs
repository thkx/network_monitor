use super::Monitor;
use crate::domain::{
    AdvancedAvailability, BasicAvailability, CertificateInfo, ContentVerificationResult,
    ContentVerificationRules, ContentVerificationRulesResult, ContentVerificationRulesSingle,
    HttpBody, HttpMethodTypes, HttpMonitorResult, MonitorType, PerformanceTimings, SecurityHeaders,
    StatusCategory, StatusInfo,
};
use crate::domain::{
    CheckResultDetail, HttpMonitorConfig, MonitorConfig, MonitorConfigDetail, UnknownMonitorResult,
};
use crate::tools::get_dns_tcp_tls_performance;
use regex::Regex;
use reqwest::Client;
use std::collections::HashMap;
use std::time::{Duration, Instant};
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
    // 按 method/headers/body 组装一个待发送的 HTTP 请求（依赖常驻 client，故为方法）
    pub fn build_request(&self, config: &HttpMonitorConfig) -> reqwest::RequestBuilder {
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
}

// 创建一个生成基础监控结果的函数
// 解析成基础的响应结果结构体
fn create_basic_result(
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
        // 字符集从 Content-Type 的 charset 参数解析（如 "text/html; charset=utf-8"）；
        // 此前误取 Accept-Charset —— 那是请求头，响应里几乎不出现，字段恒为 None
        res_charset: headers
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .and_then(charset_from_content_type),
    }
}

// 创建一个生成安全头监控结果的函数
// 解析相关头信息
fn create_headers_result(headers: &reqwest::header::HeaderMap) -> SecurityHeaders {
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
        security_headers_ok: check_security_headers_ok(headers),
    }
}
//  检测是否安全头有效
fn check_security_headers_ok(headers: &reqwest::header::HeaderMap) -> bool {
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
            },
            _ => false,
        };
        get_content_verify_result(
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
fn create_advanced_availability_result(body: &str, fields: &[String]) -> AdvancedAvailability {
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

// 采集 DNS/TCP/TLS 分段耗时与证书信息：另建探测连接的近似值，
// 失败降级为默认零值（不影响主请求的可用性判定）
async fn collect_timings(target: &str) -> (u128, u128, u128, Option<CertificateInfo>) {
    get_dns_tcp_tls_performance(target)
        .await
        .unwrap_or_default()
}

// 组装"请求成功"结果：把 response 抽取出的字段 + 探测耗时 + 客户端实测耗时
// 汇聚为 HttpMonitorResult（纯函数，无 I/O，便于单测）
#[allow(clippy::too_many_arguments)]
fn assemble_success(
    detail: &HttpMonitorConfig,
    status_code: u16,
    version: reqwest::Version,
    headers: &reqwest::header::HeaderMap,
    content_length: Option<u64>,
    body: &str,
    timings: (u128, u128, u128, Option<CertificateInfo>),
    ttfb_ms: u128,
    total_time: u128,
) -> HttpMonitorResult {
    let (dns_lookup_time, tcp_connect_time, tls_handshake_time, ssl_certificate_info) = timings;
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
    HttpMonitorResult {
        basic_available: create_basic_result(status_code, version, headers, content_length),
        response_headers: create_headers_result(headers),
        performance_timings,
        certificate_info: ssl_certificate_info.unwrap_or_default(),
        content_verification: create_content_verification_result(body, &detail.rules),
        advanced_available: create_advanced_availability_result(
            body,
            &detail.business_metric_fields,
        ),
        error_message: None,
        error_kind: None,
    }
}

// 组装"请求失败"结果：保留失败原因与错误类别（此前直接丢弃 Err(e)，
// 日志只有 Failed 无法定位原因）。目标不可达体现在 is_reachable=false
fn assemble_failure(e: &reqwest::Error) -> HttpMonitorResult {
    let kind = if e.is_timeout() {
        "timeout"
    } else if e.is_connect() {
        "connect"
    } else if e.is_decode() {
        "decode"
    } else {
        "other"
    };
    HttpMonitorResult {
        basic_available: BasicAvailability {
            is_reachable: false,
            ..Default::default()
        },
        error_message: Some(e.to_string()),
        error_kind: Some(kind.to_string()),
        ..Default::default()
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
// 从 Content-Type 头解析 charset 参数：如 "text/html; charset=utf-8" → Some("utf-8")
// 参数名大小写不敏感，值两侧的引号与空白一并去除；无 charset 参数返回 None
fn charset_from_content_type(content_type: &str) -> Option<String> {
    content_type
        .split(';')
        .filter_map(|part| part.split_once('='))
        .find(|(key, _)| key.trim().eq_ignore_ascii_case("charset"))
        .map(|(_, value)| value.trim().trim_matches('"').to_string())
        .filter(|v| !v.is_empty())
}

//  下面的话 就是要实现具体的check方法了
#[async_trait::async_trait]
impl Monitor for HttpMonitor {
    async fn check(&self, config: &MonitorConfig) -> (bool, CheckResultDetail) {
        // check() 只做编排：校验入参 → 发请求 → 采集耗时 → 抽字段 → 交给 assemble_* 汇聚
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
                // 发送HTTP请求到Target地址
                let request_client = self.build_request(detail);
                // 记录请求开始时间，用于统计总耗时等性能指标
                let start_time_instant = Instant::now();
                // 发送请求，来匹配返回的结果
                match request_client.send().await {
                    Ok(response) => {
                        // send() 返回即已收到响应头，此前耗时近似首字节时间（TTFB）
                        let ttfb_ms = start_time_instant.elapsed().as_millis();
                        // 另建探测连接采集 dns/tcp/tls 近似耗时与证书（可按配置关闭省开销）
                        let timings = if detail.collect_timings {
                            collect_timings(config.target.as_ref().unwrap()).await
                        } else {
                            Default::default()
                        };
                        // response 后续被 text() 取走所有权，这里先抽出需要的字段
                        let status_code = response.status().as_u16();
                        let version = response.version();
                        let headers = response.headers().clone();
                        let content_length = response.content_length();
                        let body = response.text().await.unwrap_or_default();
                        let total_time = start_time_instant.elapsed().as_millis();
                        (
                            true, // 监控任务执行成功；目标是否可用看 basic_available.is_reachable
                            CheckResultDetail::Http(assemble_success(
                                detail,
                                status_code,
                                version,
                                &headers,
                                content_length,
                                &body,
                                timings,
                                ttfb_ms,
                                total_time,
                            )),
                        )
                    }
                    Err(e) => (
                        true, // 监控任务执行成功；目标不可达体现在 is_reachable=false
                        CheckResultDetail::Http(assemble_failure(&e)),
                    ),
                }
            }
            _ => {
                // 如果不是 HTTP 监控类型，返回错误结果
                (
                    false, // 监控状态失败 根本就没有进入监控当中去
                    CheckResultDetail::Unknown(UnknownMonitorResult {
                        description: "调用监控类型: HTTP, 请查实后再继续操作".to_string(),
                        query_type: MonitorType::Http,
                    }),
                )
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
    use super::{HttpMonitor, charset_from_content_type, extract_json_path};
    use crate::domain::{CheckResultDetail, HttpMonitorConfig, MonitorConfig, MonitorConfigDetail};
    use crate::domain::{
        ContentVerificationRules, ContentVerificationRulesSingle, HttpMethodTypes, MonitorType,
    };
    use crate::monitor::Monitor;

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
                collect_timings: true,
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
            .check(&http_config(
                addr,
                vec![rule(ContentVerificationRules::Contains, "hello")],
                vec![],
            ))
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
            .check(&http_config(
                addr,
                vec![rule(ContentVerificationRules::NotContains, "hello")],
                vec![],
            ))
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
            r.advanced_available
                .business_metrics
                .get("code")
                .map(String::as_str),
            Some("0")
        );
        assert_eq!(
            r.advanced_available
                .business_metrics
                .get("data.queue")
                .map(String::as_str),
            Some("42")
        );
        assert!(
            !r.advanced_available
                .business_metrics
                .contains_key("missing.path")
        );
    }

    // 连接失败：任务执行成功但目标不可达，错误被分类并携带原因
    #[tokio::test]
    async fn connection_refused_marks_unreachable() {
        // 目标用 RFC 5737 TEST-NET-1（192.0.2.0/24，保留为文档用途、全球不可路由）：
        // 连接必然失败（超时或网络不可达），且不依赖任何本机端口——
        // 旧写法硬编码端口9 / 释放临时端口都受环境或并行测试端口复用影响而偶发失败。
        // 短超时（1.5s）保证连接类失败快速返回，不拖慢测试
        let config = MonitorConfig {
            target: Some("http://192.0.2.1".to_string()),
            interval: Some(60),
            monitor_type: MonitorType::Http,
            timeout: 1500,
            details: MonitorConfigDetail::Http(HttpMonitorConfig {
                url: "http://192.0.2.1".to_string(),
                method: HttpMethodTypes::Get,
                timeout: 1500,
                headers: None,
                body: None,
                rules: None,
                business_metric_fields: vec![],
                collect_timings: true,
            }),
        };
        let (status, detail) = HttpMonitor::new().check(&config).await;
        assert!(status, "任务本身执行成功，不可达体现在结果里");
        let CheckResultDetail::Http(r) = detail else {
            panic!("应为HTTP结果");
        };
        assert!(!r.basic_available.is_reachable, "不可路由地址应判为不可达");
        // 失败方式随环境而异：连接被拒为connect，无路由/被丢弃则表现为timeout。
        // 关键断言是"失败被分类并携带原因"，不假设具体的失败类别
        assert!(
            r.error_kind.is_some(),
            "失败结果应携带错误类别: {:?}",
            r.error_kind
        );
        assert!(r.error_message.is_some(), "失败结果应携带错误原因");
    }

    // collect_timings=false：跳过 dns/tcp/tls 探测连接，分段耗时为 0，
    // 但客户端实测的 ttfb/total 不受影响，可用性判定照常
    #[tokio::test]
    async fn collect_timings_false_skips_probe() {
        let addr = spawn_fake_http(canned_200("text/plain", "ok"));
        let mut cfg = http_config(addr, vec![], vec![]);
        if let MonitorConfigDetail::Http(ref mut d) = cfg.details {
            d.collect_timings = false;
        }
        let (_, detail) = HttpMonitor::new().check(&cfg).await;
        let CheckResultDetail::Http(r) = detail else {
            panic!("应为HTTP结果");
        };
        assert!(r.basic_available.is_reachable);
        assert_eq!(r.performance_timings.dns_lookup_time, 0);
        assert_eq!(r.performance_timings.tcp_connect_time, 0);
        assert_eq!(r.performance_timings.tls_handshake_time, 0);
        assert!(r.performance_timings.total_time >= r.performance_timings.first_byte_time);
    }

    // collect_timings 的 serde 默认：缺省字段应回落为 true（保持既有行为），显式 false 生效
    #[test]
    fn collect_timings_serde_default_is_true() {
        use crate::domain::MonitorDefinition;
        let omitted: MonitorDefinition =
            serde_json::from_str(r#"{"target":"https://x.com","monitor_type":"HTTP"}"#).unwrap();
        assert!(omitted.collect_timings, "缺省应为 true");
        let explicit: MonitorDefinition = serde_json::from_str(
            r#"{"target":"https://x.com","monitor_type":"HTTP","collect_timings":false}"#,
        )
        .unwrap();
        assert!(!explicit.collect_timings, "显式 false 应生效");
    }

    // charset 从 Content-Type 参数解析：大小写不敏感、去引号与空白、无参数返回 None
    #[test]
    fn charset_parsed_from_content_type_param() {
        assert_eq!(
            charset_from_content_type("text/html; charset=utf-8").as_deref(),
            Some("utf-8")
        );
        // 参数名大小写不敏感 + 值带引号与空白
        assert_eq!(
            charset_from_content_type("text/html; CharSet=\"GBK\" ").as_deref(),
            Some("GBK")
        );
        // 无 charset 参数
        assert_eq!(charset_from_content_type("application/json"), None);
        // 有分号但无 charset
        assert_eq!(charset_from_content_type("text/plain; boundary=x"), None);
        // charset= 后为空
        assert_eq!(charset_from_content_type("text/plain; charset="), None);
    }

    // 端到端：响应带 charset 的 Content-Type，结果的 res_charset 应被正确填充
    #[tokio::test]
    async fn res_charset_extracted_from_response_content_type() {
        let addr = spawn_fake_http(canned_200("text/html; charset=utf-8", "<html/>"));
        let (_, detail) = HttpMonitor::new()
            .check(&http_config(addr, vec![], vec![]))
            .await;
        let CheckResultDetail::Http(r) = detail else {
            panic!("应为HTTP结果");
        };
        assert_eq!(r.basic_available.res_charset.as_deref(), Some("utf-8"));
    }

    // 纯函数 assemble_success：客户端耗时计算与证书校验（无需真实网络）
    #[test]
    fn assemble_success_computes_client_timings() {
        use super::assemble_success;
        let detail = HttpMonitorConfig {
            url: "http://x".to_string(),
            method: HttpMethodTypes::Get,
            timeout: 1000,
            headers: None,
            body: None,
            rules: None,
            business_metric_fields: vec![],
            collect_timings: true,
        };
        let headers = reqwest::header::HeaderMap::new();
        let r = assemble_success(
            &detail,
            200,
            reqwest::Version::HTTP_11,
            &headers,
            Some(5),
            "",
            (1, 2, 3, None), // dns/tcp/tls 探测值 + 无证书
            40,              // ttfb
            100,             // total
        );
        assert_eq!(r.basic_available.res_status_code, Some(200));
        assert!(r.basic_available.is_reachable);
        assert_eq!(r.performance_timings.dns_lookup_time, 1);
        assert_eq!(r.performance_timings.first_byte_time, 40);
        assert_eq!(r.performance_timings.content_download_time, 60); // total-ttfb
        assert_eq!(r.performance_timings.total_time, 100);
        assert!(!r.performance_timings.ssl_cert_valid, "无证书时应为 false");
        assert!(r.error_kind.is_none());
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
