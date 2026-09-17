// http相关
use std::collections::HashMap;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum HttpMethodTypes {
    #[serde(rename = "GET")]
    Get,
    #[serde(rename = "POST")]
    Post,
    #[serde(rename = "PUT")]
    Put,
    #[serde(rename = "DELETE")]
    Delete,
    #[serde(rename = "HEAD")]
    Head,
    #[serde(rename = "OPTIONS")]
    Options,
    #[serde(rename = "PATCH")]
    Patch,
    #[serde(rename = "TRACE")]
    Trace,
    #[serde(rename = "CONNECT")]
    Connect,
}
#[derive(Debug, Clone)]
pub enum HttpBody {
    Text(String),
    Binary(Vec<u8>),
    Json(serde_json::Value),
    Empty,
}

// 监控类型
#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
pub enum MonitorType {
    #[serde(rename = "ICMP")]
    Icmp,
    #[serde(rename = "TCP")]
    Tcp,
    #[serde(rename = "UDP")]
    Udp,
    #[serde(rename = "DNS")]
    Dns,
    #[serde(rename = "HTTP")]
    #[default]
    Http,
    #[serde(rename = "FTP")]
    Ftp,
    #[serde(rename = "TRACEROUTE")]
    Traceroute,
    #[serde(rename = "CPU")]
    Cpu,
    #[serde(rename = "MEMORY")]
    Memory,
    #[serde(rename = "DISK")]
    Disk,
    #[serde(rename = "PROCESS")]
    Process,
    #[serde(rename = "UNKNOWN")]
    Unknown,
}

impl std::fmt::Display for MonitorType {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match *self {
            MonitorType::Icmp => write!(f, "ICMP"),
            MonitorType::Tcp => write!(f, "TCP"),
            MonitorType::Udp => write!(f, "UDP"),
            MonitorType::Dns => write!(f, "DNS"),
            MonitorType::Http => write!(f, "HTTP"),
            MonitorType::Ftp => write!(f, "FTP"),
            MonitorType::Traceroute => write!(f, "TRACEROUTE"),
            MonitorType::Cpu => write!(f, "CPU"),
            MonitorType::Memory => write!(f, "MEMORY"),
            MonitorType::Disk => write!(f, "DISK"),
            MonitorType::Process => write!(f, "PROCESS"),
            MonitorType::Unknown => write!(f, "UNKNOWN"),
        }
    }
}

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
//定义内容监控规则结构体明细字段
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ContentVerificationRulesSingle {
    pub rule_type: ContentVerificationRules,
    pub rule_content: String,
    pub rule_description: String,
}
// 内容验证类别
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[derive(Default)]
pub enum ContentVerificationRules {
    #[serde(rename = "contains")]
    Contains, // 响应内容中包含指定字符串
    #[serde(rename = "not_contains")]
    NotContains, // 响应内容中不包含指定字符串
    #[serde(rename = "regex")]
    Regex, // 响应内容匹配正则表达式
    // 定义一个默认值 用于匹配
    #[serde(rename = "default")] // 定义默认值
    #[default]
    Default,
}
// 设置默认值

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[derive(Default)]
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


/// 自定义监控配置，用于从JSON文件中读取监控任务列表（每个监控引擎的参数都是一个对象）
/// 同时也是Web API创建/更新监控配置时请求体的结构
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SelfDefineMonitorConfig {
    pub target: Option<String>,        // 监控的目标URL或IP地址
    #[serde(default)]
    pub monitor_type: MonitorType,     // 监控类型，缺省为HTTP
    #[serde(default)]
    pub method: Option<HttpMethodTypes>, // HTTP请求方法，缺省为GET
    #[serde(default)]
    pub params: serde_json::Map<String, serde_json::Value>,    // 请求参数：GET类请求追加为URL查询参数，POST类请求作为JSON请求体
    #[serde(default)]
    pub headers: HashMap<String, String>, // 自定义请求头
    #[serde(default)]
    pub body: Option<HttpBodyConfig>, // 显式请求体配置，优先级高于params的JSON body语义
    #[serde(default)]
    pub content_evaluation_rules: Option<Vec<ContentVerificationRulesSingle>>, // 内容验证规则
    /// 业务指标提取字段（JSON点路径，如 "code"、"data.queue"）：
    /// HTTP响应体为JSON时按路径提取值到 advanced_available.business_metrics，
    /// 供结果详情/告警消息展示（如接口返回的内部业务码、队列深度等）
    #[serde(default)]
    pub business_metric_fields: Vec<String>,
    #[serde(default)]
    pub alert_rules: Option<AlertVerificationRules>, // 告警配置
    #[serde(default)]
    pub interval: Option<u64>,         // 监控间隔，单位秒
    #[serde(default)]
    pub timeout: Option<u64>,          // HTTP监控超时时间，单位毫秒
    #[serde(default)]
    pub description: Option<String>,   // 描述信息
}

// 监控项的展示名称：优先使用target，没有target时（如CPU/MEMORY/DISK监控）使用监控类型名
// （此前住在main.rs，scheduler/api需要跨层引用，随配置展示语义归位到类型模块）
pub fn display_name(entry: &SelfDefineMonitorConfig) -> String {
    entry
        .target
        .clone()
        .unwrap_or_else(|| entry.monitor_type.to_string())
}

/// 告警配置：通知类型 + 通知渠道 + 告警触发规则列表
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AlertVerificationRules {
    pub notify_type: String,               // 通知类型，目前支持 FEISHU
    pub notify_config: NotifyConfig,       // 通知渠道配置
    #[serde(default)]
    pub rules: Vec<AlertSingleRule>,       // 告警触发规则列表
    /// 告警防抖：连续 N 次命中规则才发送告警（缺省1=首次命中即告警；根治网络抖动误报）
    #[serde(default)]
    pub consecutive_failures: Option<u32>,
    /// 恢复防抖：连续 M 次正常才发送恢复通知（缺省1；防止单次成功误报恢复）
    #[serde(default)]
    pub consecutive_successes: Option<u32>,
}

/// 告警通知渠道配置
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct NotifyConfig {
    /// Webhook地址：FEISHU/DINGTALK/WECOM渠道必填（validate_config校验非空），
    /// EMAIL渠道不使用——serde(default)使其无需填占位值
    #[serde(default)]
    pub webhook_url: String,
    /// 签名密钥（可选）：FEISHU/DINGTALK 后台开启"签名校验"时必填，
    /// 引擎自动计算签名——飞书附加到请求体，钉钉自动拼接到 webhook_url
    #[serde(default)]
    pub secret: Option<String>,
    /// SMTP邮件配置（可选）：notify_type=EMAIL 时必填，其余渠道忽略
    #[serde(default)]
    pub email: Option<EmailNotifyConfig>,
}

/// EMAIL渠道的SMTP配置：直连SMTP服务器发信（465隐式TLS / 587 STARTTLS / 25明文）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EmailNotifyConfig {
    pub smtp_host: String,          // SMTP服务器地址，如 smtp.example.com
    #[serde(default = "default_smtp_port")]
    pub smtp_port: u16,             // 465=隐式TLS，587=STARTTLS，25=明文
    pub username: String,           // 认证用户名（通常为发件邮箱）
    #[serde(default)]
    pub password: String,           // 认证密码/授权码；为空则跳过认证（本地relay场景）
    #[serde(default)]
    pub from: Option<String>,       // 发件人；缺省使用username
    pub to: Vec<String>,            // 收件人列表（至少一个）
}

fn default_smtp_port() -> u16 {
    465
}

/// 单条告警触发规则
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AlertSingleRule {
    pub rule_type: AlertRuleTypes, // 规则类型，目前支持 RESPONSE_CODE
    pub condition: NotifyCondition, // 触发条件
}

/// 告警规则类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AlertRuleTypes {
    #[serde(rename = "RESPONSE_CODE")]
    ResponseCode, // 响应码规则（仅HTTP）
    #[serde(rename = "CONTENT")]
    Content, // 内容匹配规则（仅HTTP：内容校验规则存在未命中项时告警）
    #[serde(rename = "AVAILABILITY")]
    Availability, // 可用性规则（全部监控类型生效）
    #[serde(rename = "THRESHOLD")]
    Threshold, // 阈值规则（系统资源类：CPU/内存/磁盘/进程）
}

/// 告警触发条件
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct NotifyCondition {
    #[serde(default)]
    pub no_contains: Vec<u16>, // 响应码不在列表内则触发告警
    #[serde(default)]
    pub contains: Vec<u16>,    // 响应码在列表内则触发告警
    #[serde(default)]
    pub regex: String,         // 响应码正则匹配
    /// 阈值条件（THRESHOLD规则用）：如 {"metric":"cpu","op":">","value":80}
    #[serde(default)]
    pub threshold: Option<ThresholdCondition>,
}

/// 阈值条件：metric为资源类别，op为比较符，value为阈值
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ThresholdCondition {
    pub metric: String, // cpu / memory / disk（使用率）/ available_bytes / process
    pub op: String,     // > >= < <= ==
    pub value: f64,     // 阈值
}

/// HTTP请求体配置（JSON配置文件/API请求体中的表达形式）
/// 采用internally-tagged枚举，JSON形如 {"type": "...", "content": ...}：
/// - text:   {"type":"text","content":"原始文本"}       → HttpBody::Text，未指定Content-Type时默认text/plain
/// - json:   {"type":"json","content":{...}}           → HttpBody::Json
/// - binary: {"type":"binary","content":"<base64>"}    → HttpBody::Binary，未指定Content-Type时默认application/octet-stream
/// - empty:  {"type":"empty"}                          → HttpBody::Empty，显式发送Content-Length: 0的空body
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum HttpBodyConfig {
    Text {
        content: String,
    },
    Json {
        content: serde_json::Value,
    },
    Binary {
        content: String, // base64编码的字节内容（JSON无法直接携带原始字节）
    },
    Empty,
}