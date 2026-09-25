// 探测产出结果族：HTTP 探测执行后的各类结果结构（输出侧，仅 Serialize 落库/回显）。
// 与 config（用户输入配置）关注点分离——结果结构随探测能力演进，配置结构随 API 契约演进。
use super::config::ContentVerificationRulesSingle;
use std::collections::HashMap;

// 定义一个struct 来保存http相关监控的最终结果参数结构体
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct HttpMonitorResult {
    pub basic_available: BasicAvailability,
    pub response_headers: SecurityHeaders,
    pub performance_timings: PerformanceTimings,
    pub certificate_info: CertificateInfo, // SSL证书信息
    pub content_verification: ContentVerificationResult,
    pub advanced_available: AdvancedAvailability,
    pub error_message: Option<String>, // 请求失败原因（成功时为None；此前失败路径直接丢弃错误信息）
    pub error_kind: Option<String>,    // 错误类别：timeout/connect/decode/other
}

// 定义一个struct 来保存基本的HTTP可用性信息
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct BasicAvailability {
    pub is_reachable: bool,        // 是否可达
    pub dns_resolvable: bool,      // DNS解析是否成功
    pub tcp_connect_success: bool, // TCP链接是否成功
    // HTTP相关
    pub res_received: bool,                  // 是否收到响应
    pub res_status_code: Option<u16>,        // 响应状态码
    pub res_status_category: StatusCategory, // 响应状态码类别
    pub protocol_version: String,            // HTTP协议版本（如HTTP/1.1, HTTP/2）
    pub res_content_type: Option<String>,    // 响应内容类型
    pub res_content_length: u64,             // 响应内容长度
    pub res_charset: Option<String>,         // 响应字符集
}

// 定义一个struct 来保存安全头监控信息
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct SecurityHeaders {
    pub strict_transport_security: Option<String>, // 是否存在Strict-Transport-Security头
    pub content_security_policy: Option<String>,   // 是否存在Content-Security-Policy头
    pub x_content_type_options: Option<String>,    // 是否存在X-Content-Type-Options头
    pub x_frame_options: Option<String>,           // 是否存在X-Frame-Options头
    pub x_xss_protection: Option<String>,          // 是否存在X-XSS-Protection头
    pub referrer_policy: Option<String>,           // 是否存在Referrer-Policy头
    pub feature_policy: Option<String>,            // 是否存在Feature-Policy头
    pub permissions_policy: Option<String>,        // 是否存在Permissions-Policy头
    pub security_headers_ok: bool,                 // 是否所有安全头都存在
}

// 创建一个枚举类型，表示性能指标的类别
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct PerformanceTimings {
    // 性能指标相关
    pub dns_lookup_time: u128,         // DNS查询时间，单位毫秒
    pub tcp_connect_time: u128,        // TCP连接时间，单位毫秒
    pub tls_handshake_time: u128,      // TLS握手时间，单位毫秒
    pub first_byte_time: u128,         // 首字节时间，单位毫秒
    pub content_download_time: u128,   // 内容下载时间，单位毫秒
    pub ssl_negotiation_time: u128,    // SSL协商时间，单位毫秒
    pub ssl_cert_valid: bool,          // SSL证书是否有效
    pub server_processing_time: u128,  // 服务器处理时间，单位毫秒
    pub response_receiving_time: u128, // 响应接收时间，单位毫秒
    pub total_time: u128,              // 总时间，单位毫秒
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct CertificateInfo {
    pub issuer: Option<String>,               // 证书颁发者
    pub subject: Option<String>,              // 证书主题
    pub valid_from: Option<String>,           // 证书有效期开始时间
    pub valid_until: Option<String>,          // 证书有效期结束时间
    pub serial_number: Option<String>,        // 证书序列号
    pub signature_algorithm: Option<String>,  // 签名算法
    pub public_key_algorithm: Option<String>, // 公钥算法
    pub public_key_size: Option<usize>,       // 公钥大小
    pub is_valid: bool,                       // 证书是否有效
}

//  创建一个内容验证结果
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ContentVerificationResult {
    pub match_rules: Vec<ContentVerificationRulesResult>, // 匹配的规则
    pub failed_rules: Vec<ContentVerificationRulesResult>, // 未匹配的规则 及其原因
}

// 定义核心内容验证返回数据结构体
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ContentVerificationRulesResult {
    pub rule_id: u64,
    pub status: StatusInfo,
    pub message: String,
    pub rules: ContentVerificationRulesSingle,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub enum StatusInfo {
    Success,
    Failed,
    #[default]
    Unknown,
}

// 高级可用性类别 业务指标监控 事务监控 等
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct AdvancedAvailability {
    pub business_metrics: HashMap<String, String>,
}

// 定义状态码对应的状态枚举类型
#[derive(Debug, Clone, Default, serde::Serialize)]
pub enum StatusCategory {
    Informational, // 1xx 信息响应
    Success,       // 2xx 成功响应
    Redirection,   // 3xx 重定向
    ClientError,   // 4xx 客户端错误
    ServerError,   // 5xx 服务器错误
    #[default]
    Unknown, // 未知状态
}

// 给对应的枚举值 添加方法 来获取不同的状态码的实际状态
impl StatusCategory {
    pub fn from_status_code(status_code: u16) -> Self {
        match status_code {
            100..=199 => StatusCategory::Informational,
            200..=299 => StatusCategory::Success,
            300..=399 => StatusCategory::Redirection,
            400..=499 => StatusCategory::ClientError,
            500..=599 => StatusCategory::ServerError,
            _ => StatusCategory::Unknown,
        }
    }
}
