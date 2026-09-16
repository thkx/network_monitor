//  类型声明模块
// MonitorConfig::from_entry：JSON配置项 → 运行时监控配置的唯一转换点
// （此前住在main.rs，导致api/scheduler对crate root的逆向依赖）

use crate::tools_types::{
    HttpBody, HttpBodyConfig, HttpMethodTypes, HttpMonitorResult, MonitorType,
    SelfDefineMonitorConfig,
};
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine as _;
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};

// 内容验证规则结构体，全局统一定义在 tools_types 中，这里直接重导出使用
pub use crate::tools_types::ContentVerificationRulesSingle;

// 定义核心的监控配置参数结构体
#[derive(Debug, Clone)]
pub struct MonitorConfig {
    pub target: Option<String>,       // 监控的目标URL或IP地址
    pub interval: Option<u64>,        // 监控间隔，单位秒
    pub monitor_type: MonitorType,    // 监控类型 HTTP TCP FTP等等
    pub details: MonitorConfigDetail, // 这里的话是不同监控类型的具体参数，跟公共的一些参数区分开 详见14行代码声明
}

impl MonitorConfig {
    // 从JSON配置项构建运行时监控配置（配置转换的唯一位置）
    // default_interval：配置项未指定interval时使用的默认监控间隔（来自命令行参数）
    pub fn from_entry(entry: &SelfDefineMonitorConfig, default_interval: u64) -> Self {
        let monitor_type = entry.monitor_type;
        // 按监控类型构建各自的详情参数
        let details = match monitor_type {
            MonitorType::Http => {
                // HTTP请求方法，未配置时默认GET
                let method = entry.method.clone().unwrap_or(HttpMethodTypes::Get);
                let mut url = entry.target.clone().unwrap_or_default();
                // GET/HEAD类请求的params追加为URL查询参数（form_urlencoded做URL编码，值含&/=/空格/中文等特殊字符也安全）
                if !entry.params.is_empty()
                    && matches!(method, HttpMethodTypes::Get | HttpMethodTypes::Head)
                {
                    let pairs: Vec<(String, String)> = entry
                        .params
                        .iter()
                        .map(|(k, v)| {
                            // 字符串值取原文字面量，其他JSON类型用其JSON文本表示
                            let val = match v {
                                serde_json::Value::String(s) => s.clone(),
                                other => other.to_string(),
                            };
                            (k.clone(), val)
                        })
                        .collect();
                    let query = url::form_urlencoded::Serializer::new(String::new())
                        .extend_pairs(pairs)
                        .finish();
                    let sep = if url.contains('?') { "&" } else { "?" };
                    url = format!("{}{}{}", url, sep, query);
                }
                // headers配置转换为HeaderMap，非法的键值直接忽略
                let mut header_map = HeaderMap::new();
                for (k, v) in &entry.headers {
                    if let (Ok(name), Ok(value)) = (
                        reqwest::header::HeaderName::from_bytes(k.as_bytes()),
                        HeaderValue::from_str(v),
                    ) {
                        header_map.insert(name, value);
                    }
                }
                // 用户已显式配置Content-Type时不再补充默认值
                let has_content_type = entry
                    .headers
                    .keys()
                    .any(|k| k.eq_ignore_ascii_case("content-type"));
                // 请求体构造：显式body配置优先；未配置body时保留旧的params语义（POST类请求params作为JSON请求体）
                let body = if let Some(body_cfg) = entry.body.as_ref() {
                    match body_cfg {
                        HttpBodyConfig::Text { content } => {
                            if !has_content_type {
                                header_map.insert(
                                    CONTENT_TYPE,
                                    HeaderValue::from_static("text/plain"),
                                );
                            }
                            Some(HttpBody::Text(content.clone()))
                        }
                        HttpBodyConfig::Json { content } => {
                            Some(HttpBody::Json(content.clone()))
                        }
                        HttpBodyConfig::Binary { content } => {
                            // binary的content为base64编码，解码失败时忽略该body并告警
                            match BASE64_STANDARD.decode(content.as_bytes()) {
                                Ok(bytes) => {
                                    if !has_content_type {
                                        header_map.insert(
                                            CONTENT_TYPE,
                                            HeaderValue::from_static("application/octet-stream"),
                                        );
                                    }
                                    Some(HttpBody::Binary(bytes))
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        "body(binary) base64解码失败，本次请求不携带body: {}",
                                        e
                                    );
                                    None
                                }
                            }
                        }
                        HttpBodyConfig::Empty => Some(HttpBody::Empty),
                    }
                } else if !entry.params.is_empty()
                    && matches!(
                        method,
                        HttpMethodTypes::Post | HttpMethodTypes::Put | HttpMethodTypes::Patch
                    )
                {
                    Some(HttpBody::Json(serde_json::Value::Object(
                        entry.params.clone(),
                    )))
                } else {
                    None
                };
                let headers = if header_map.is_empty() {
                    None
                } else {
                    Some(header_map)
                };
                MonitorConfigDetail::Http(HttpMonitorConfig {
                    url,
                    method,
                    timeout: entry.timeout.unwrap_or(5000),
                    headers,
                    body,
                    rules: entry.content_evaluation_rules.clone(),
                })
            }
            MonitorType::Icmp => MonitorConfigDetail::Icmp(IcmpMonitorConfig {}),
            MonitorType::Tcp => MonitorConfigDetail::Tcp(TcpMonitorConfig {}),
            MonitorType::Udp => MonitorConfigDetail::Udp(UdpMonitorConfig {}),
            MonitorType::Dns => MonitorConfigDetail::Dns(DnsMonitorConfig {}),
            MonitorType::Ftp => MonitorConfigDetail::Ftp(FtpMonitorConfig {}),
            MonitorType::Traceroute => {
                MonitorConfigDetail::Traceroute(TracerouteMonitorConfig {})
            }
            MonitorType::Cpu => MonitorConfigDetail::Cpu(CpuMonitorConfig {}),
            MonitorType::Memory => MonitorConfigDetail::Memory(MemoryMonitorConfig {}),
            MonitorType::Disk => MonitorConfigDetail::Disk(DiskMonitorConfig {}),
            MonitorType::Process => MonitorConfigDetail::Process(ProcessMonitorConfig {}),
            MonitorType::Unknown => MonitorConfigDetail::Unknown(UnknownQueryConfig {
                description: "未知监控类型".to_string(),
            }),
        };
        MonitorConfig {
            target: entry.target.clone(),
            interval: Some(entry.interval.unwrap_or(default_interval)),
            monitor_type,
            details,
        }
    }
}

// 定义一个监控数据的详情监控参数 结构体 ，用于每个监控类型中的不同的参数类型
#[derive(Debug, Clone)]
pub enum MonitorConfigDetail {
    Http(HttpMonitorConfig),
    Ftp(FtpMonitorConfig),
    Traceroute(TracerouteMonitorConfig),
    Cpu(CpuMonitorConfig),
    Memory(MemoryMonitorConfig),
    Disk(DiskMonitorConfig),
    Process(ProcessMonitorConfig),
    Icmp(IcmpMonitorConfig),
    Tcp(TcpMonitorConfig),
    Udp(UdpMonitorConfig),
    Dns(DnsMonitorConfig),
    Unknown(UnknownQueryConfig),
}
// 其中这些不同监控类型的机构体我们就不一一声明了 ，都是类似的，
// 异常监控的兜底类型
#[derive(Debug, Clone)]
pub struct UnknownQueryConfig {
    pub description: String, // 异常描述信息
}

#[derive(Debug, Clone)]
pub struct IcmpMonitorConfig {}
#[derive(Debug, Clone)]
pub struct TcpMonitorConfig {}
#[derive(Debug, Clone)]
pub struct UdpMonitorConfig {}
#[derive(Debug, Clone)]
pub struct DnsMonitorConfig {}
#[derive(Debug, Clone)]
pub struct FtpMonitorConfig {}

#[derive(Debug, Clone)]
pub struct TracerouteMonitorConfig {}

#[derive(Debug, Clone)]
pub struct DiskMonitorConfig {}

#[derive(Debug, Clone)]
pub struct ProcessMonitorConfig {}

#[derive(Debug, Clone)]
pub struct MemoryMonitorConfig {}

#[derive(Debug, Clone)]
pub struct CpuMonitorConfig {}

// HTTP监控的配置参数
#[derive(Debug, Clone)]
pub struct HttpMonitorConfig {
    pub url: String,                                        // 定义HTTP监控的URL
    pub method: HttpMethodTypes,                            // GET, POST, etc.
    pub timeout: u64,                                       // 配置超时时间
    pub headers: Option<HeaderMap<HeaderValue>>,            // 可选的请求URL需要的HTTP头
    pub body: Option<HttpBody>,                             // 可选的请求URL需要的body体
    pub rules: Option<Vec<ContentVerificationRulesSingle>>, // 可配置的监控规则
}

// 其中监控的规则的话我们来简单实现了一下
//定义内容监控规则结构体明细字段

// 定义核心的监控结果结构体
// id: 本次监控的唯一标识
// monitor_type: 监控类型
// target: 监控的目标
// status: 监控任务本身是否执行成功（注意与目标是否可用区分）
// details: 具体监控类型的详细结果数据
#[derive(Debug, Clone)]
pub struct CheckResult {
    pub id: u128,
    pub monitor_type: MonitorType,
    pub target: Option<String>,
    pub status: bool,
    pub details: CheckResultDetail,
}

// 定义核心的监控结果详情枚举，每种监控类型对应一个具体的结果结构体
// allow：Http变体字段最全（约800字节）远大于其他变体；各处均按值构造/匹配，
// 装箱会波及所有调用点，收益不大，此处显式豁免
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, serde::Serialize)]
pub enum CheckResultDetail {
    Http(HttpMonitorResult),
    Ftp(FtpMonitorResult),
    Traceroute(TracerouteMonitorResult),
    Cpu(CpuMonitorResult),
    Memory(MemoryMonitorResult),
    Disk(DiskMonitorResult),
    Process(ProcessMonitorResult),
    Icmp(IcmpMonitorResult),
    Tcp(TcpMonitorResult),
    Udp(UdpMonitorResult),
    Dns(DnsMonitorResult),
    Unknown(UnknownMonitorResult),
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct FtpMonitorResult {
    pub connected: bool,        // FTP连接是否成功
    pub banner: Option<String>, // 服务端返回的欢迎横幅信息
    pub elapsed_ms: u128,       // 连接耗时，单位毫秒
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct TracerouteMonitorResult {
    pub success: bool,     // 是否成功追踪到目标
    pub hops: Vec<String>, // 每一跳的明细信息
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct CpuMonitorResult {
    pub usage_percent: f32, // CPU总体使用率（%）
    pub core_count: usize,  // CPU逻辑核心数
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct MemoryMonitorResult {
    pub total_bytes: u64,   // 总内存（字节）
    pub used_bytes: u64,    // 已使用内存（字节）
    pub usage_percent: f32, // 内存使用率（%）
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct DiskMonitorResult {
    pub total_bytes: u64,     // 所有磁盘总容量（字节）
    pub available_bytes: u64, // 所有磁盘剩余可用容量（字节）
    pub disks: Vec<DiskInfo>, // 每块磁盘的明细信息
}

// 单块磁盘的监控信息
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct DiskInfo {
    pub name: String,         // 磁盘名称
    pub mount_point: String,  // 挂载点
    pub total_bytes: u64,     // 总容量（字节）
    pub available_bytes: u64, // 剩余可用容量（字节）
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ProcessMonitorResult {
    pub process_count: usize,             // 系统进程总数
    pub top_by_memory: Vec<ProcessBrief>, // 按内存占用排序的前10个进程
}

// 进程简要信息
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ProcessBrief {
    pub pid: u32,          // 进程ID
    pub name: String,      // 进程名称
    pub memory_bytes: u64, // 内存占用（字节）
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct IcmpMonitorResult {
    pub is_alive: bool,   // 目标主机是否存活
    pub elapsed_ms: u128, // ping耗时，单位毫秒
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct UdpMonitorResult {
    pub sent: bool,              // 探测包是否发送成功
    pub response_received: bool, // 是否收到UDP响应
    pub elapsed_ms: u128,        // 往返耗时，单位毫秒
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct DnsMonitorResult {
    pub resolved: bool,   // 域名是否解析成功
    pub elapsed_ms: u128, // 解析耗时，单位毫秒
    pub ips: Vec<String>, // 解析到的IP地址列表
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UnknownMonitorResult {
    pub description: String,
    pub query_type: MonitorType,
}

impl Default for UnknownMonitorResult {
    fn default() -> Self {
        Self {
            description: String::from("No description"),
            query_type: MonitorType::Unknown,
        }
    }
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct TcpMonitorResult {
    pub connected: bool,  // TCP连接是否成功
    pub elapsed_ms: u128, // 连接耗时，单位毫秒
}

impl CheckResult {
    // 把不同监控类型的结果统一转换为日志/持久化所需字段：(状态, 耗时ms, 状态码)
    // 系统资源类监控（CPU/MEMORY/DISK/PROCESS等）能执行即视为成功
    pub fn log_fields(&self) -> (bool, u128, Option<u16>) {
        match &self.details {
            CheckResultDetail::Http(r) => (
                r.basic_avaliable.is_reachable,
                r.performance_timings.total_time,
                r.basic_avaliable.res_status_code,
            ),
            CheckResultDetail::Icmp(r) => (r.is_alive, r.elapsed_ms, None),
            CheckResultDetail::Tcp(r) => (r.connected, r.elapsed_ms, None),
            CheckResultDetail::Udp(r) => (r.response_received, r.elapsed_ms, None),
            CheckResultDetail::Dns(r) => (r.resolved, r.elapsed_ms, None),
            CheckResultDetail::Ftp(r) => (r.connected, r.elapsed_ms, None),
            CheckResultDetail::Traceroute(r) => (r.success, 0, None),
            _ => (self.status, 0, None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CheckResult, CheckResultDetail, MonitorConfig, MonitorConfigDetail, MonitorType,
        UnknownMonitorResult,
    };
    use crate::tools_types::{HttpBody, HttpMethodTypes, SelfDefineMonitorConfig};
    use reqwest::header::{HeaderValue, CONTENT_TYPE};

    // 从JSON构造配置项（与API请求体/monitor_list.json的实际入参路径一致）
    fn entry_from_json(json: &str) -> SelfDefineMonitorConfig {
        serde_json::from_str(json).expect("测试JSON应可反序列化")
    }

    #[test]
    fn http_get_params_are_appended_as_query() {
        let entry = entry_from_json(
            r#"{"target":"https://a.com/p","monitor_type":"HTTP","method":"GET","params":{"k":"v"}}"#,
        );
        let config = MonitorConfig::from_entry(&entry, 5);
        let MonitorConfigDetail::Http(http) = config.details else {
            panic!("HTTP配置应生成Http详情");
        };
        assert_eq!(http.url, "https://a.com/p?k=v");
        assert!(matches!(http.method, HttpMethodTypes::Get));
    }

    #[test]
    fn http_post_params_become_json_body() {
        let entry = entry_from_json(
            r#"{"target":"https://a.com/p","monitor_type":"HTTP","method":"POST","params":{"k":"v"}}"#,
        );
        let config = MonitorConfig::from_entry(&entry, 5);
        let MonitorConfigDetail::Http(http) = config.details else {
            panic!("HTTP配置应生成Http详情");
        };
        match http.body.expect("POST的params应转为JSON body") {
            HttpBody::Json(v) => assert_eq!(v["k"], "v"),
            other => panic!("应为Json body，实际: {:?}", other),
        }
    }

    #[test]
    fn explicit_text_body_overrides_params() {
        let entry = entry_from_json(
            r#"{"target":"https://a.com/p","monitor_type":"HTTP","method":"POST","params":{"k":"v"},"body":{"type":"text","content":"hi"}}"#,
        );
        let config = MonitorConfig::from_entry(&entry, 5);
        let MonitorConfigDetail::Http(http) = config.details else {
            panic!("HTTP配置应生成Http详情");
        };
        match http.body.expect("显式body应生效") {
            HttpBody::Text(content) => assert_eq!(content, "hi"),
            other => panic!("应为Text body，实际: {:?}", other),
        }
        // 未显式配置Content-Type时默认补text/plain
        let headers = http.headers.expect("应补充默认Content-Type头");
        assert_eq!(
            headers.get(CONTENT_TYPE),
            Some(&HeaderValue::from_static("text/plain"))
        );
    }

    #[test]
    fn invalid_binary_body_is_dropped() {
        let entry = entry_from_json(
            r#"{"target":"https://a.com/p","monitor_type":"HTTP","method":"POST","body":{"type":"binary","content":"!!not_base64!!"}}"#,
        );
        let config = MonitorConfig::from_entry(&entry, 5);
        let MonitorConfigDetail::Http(http) = config.details else {
            panic!("HTTP配置应生成Http详情");
        };
        // base64解码失败：不携带body，避免发出脏数据
        assert!(http.body.is_none());
    }

    #[test]
    fn interval_and_timeout_defaults_apply() {
        let entry = entry_from_json(r#"{"target":"https://a.com","monitor_type":"HTTP"}"#);
        let config = MonitorConfig::from_entry(&entry, 7);
        assert_eq!(config.interval, Some(7));
        let MonitorConfigDetail::Http(http) = config.details else {
            panic!("HTTP配置应生成Http详情");
        };
        assert_eq!(http.timeout, 5000);
        assert!(http.headers.is_none());
    }

    #[test]
    fn unknown_type_falls_back_to_unknown_detail() {
        let entry = entry_from_json(r#"{"target":"x","monitor_type":"UNKNOWN"}"#);
        let config = MonitorConfig::from_entry(&entry, 5);
        match config.details {
            MonitorConfigDetail::Unknown(u) => assert_eq!(u.description, "未知监控类型"),
            other => panic!("应为Unknown详情，实际: {:?}", other),
        }
    }

    #[test]
    fn log_fields_maps_each_result_type() {
        // HTTP：取可达性与状态码
        let http = CheckResult {
            id: 1,
            monitor_type: MonitorType::Http,
            target: None,
            status: true,
            details: CheckResultDetail::Http(crate::tools_types::HttpMonitorResult {
                basic_avaliable: crate::tools_types::BasicAvailability {
                    is_reachable: false,
                    res_status_code: Some(500),
                    ..Default::default()
                },
                ..Default::default()
            }),
        };
        assert_eq!(http.log_fields(), (false, 0, Some(500)));
        // 兜底类型：跟随任务状态
        let unknown = CheckResult {
            id: 2,
            monitor_type: MonitorType::Unknown,
            target: None,
            status: true,
            details: CheckResultDetail::Unknown(UnknownMonitorResult::default()),
        };
        assert_eq!(unknown.log_fields(), (true, 0, None));
    }
}
