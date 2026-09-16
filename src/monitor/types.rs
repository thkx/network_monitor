//  类型声明模块

use crate::tools_types::{HttpBody, HttpMethodTypes, HttpMonitorResult, MonitorType};
use reqwest::header::{HeaderMap, HeaderValue};

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
