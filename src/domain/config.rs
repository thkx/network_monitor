// 监控配置族：从 JSON / API 请求体读入的用户输入配置，以及监控类型枚举与请求相关类型。
// 与 result（探测产出）关注点分离——配置结构随 API 契约演进，结果结构随探测能力演进。
use super::alert::AlertVerificationRules;
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

impl MonitorType {
    // 大写类型名：Display 与各处日志/消息统一引用此处，避免与 serde(rename) 漂移
    pub fn as_str(&self) -> &'static str {
        match self {
            MonitorType::Icmp => "ICMP",
            MonitorType::Tcp => "TCP",
            MonitorType::Udp => "UDP",
            MonitorType::Dns => "DNS",
            MonitorType::Http => "HTTP",
            MonitorType::Ftp => "FTP",
            MonitorType::Traceroute => "TRACEROUTE",
            MonitorType::Cpu => "CPU",
            MonitorType::Memory => "MEMORY",
            MonitorType::Disk => "DISK",
            MonitorType::Process => "PROCESS",
            MonitorType::Unknown => "UNKNOWN",
        }
    }
}

impl std::fmt::Display for MonitorType {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

//定义内容监控规则结构体明细字段
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ContentVerificationRulesSingle {
    pub rule_type: ContentVerificationRules,
    pub rule_content: String,
    pub rule_description: String,
}

// 内容验证类别
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
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

/// 自定义监控配置，用于从JSON文件中读取监控任务列表（每个监控引擎的参数都是一个对象）
/// 同时也是Web API创建/更新监控配置时请求体的结构
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SelfDefineMonitorConfig {
    pub target: Option<String>, // 监控的目标URL或IP地址
    #[serde(default)]
    pub monitor_type: MonitorType, // 监控类型，缺省为HTTP
    #[serde(default)]
    pub method: Option<HttpMethodTypes>, // HTTP请求方法，缺省为GET
    #[serde(default)]
    pub params: serde_json::Map<String, serde_json::Value>, // 请求参数：GET类请求追加为URL查询参数，POST类请求作为JSON请求体
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
    pub interval: Option<u64>, // 监控间隔，单位秒
    #[serde(default)]
    pub timeout: Option<u64>, // HTTP监控超时时间，单位毫秒
    #[serde(default)]
    pub description: Option<String>, // 描述信息
    /// 分组标签（可选）：用于列表页按业务分组筛选（?tag=）；空/缺省为不分组
    #[serde(default)]
    pub tag: Option<String>,
    /// 是否采集 DNS/TCP/TLS 分段耗时（HTTP 专用，另建一次探测连接）：默认 true；
    /// 高频/大量 HTTP 监控可设 false 省去每次检查的额外探测连接，此时分段耗时为 0
    #[serde(default = "default_true")]
    pub collect_timings: bool,
}

// serde 默认值助手：collect_timings 缺省为 true（保持既有行为）
fn default_true() -> bool {
    true
}

// 监控项的展示名称：优先使用target，没有target时（如CPU/MEMORY/DISK监控）使用监控类型名
// （此前住在main.rs，scheduler/api需要跨层引用，随配置展示语义归位到类型模块）
pub fn display_name(entry: &SelfDefineMonitorConfig) -> String {
    entry
        .target
        .clone()
        .unwrap_or_else(|| entry.monitor_type.to_string())
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
