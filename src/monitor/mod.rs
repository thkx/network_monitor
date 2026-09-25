pub mod types;
use crate::domain::MonitorType; // 全局通用的类型定义模块

// 引进特征对象：CheckResultDetail/MonitorConfig 在本模块的 types 子模块（crate::monitor::types），
// 注意与 crate::domain（全局类型）区分——此处必须用 self::types，不能写 super::types
use self::types::{CheckResultDetail, MonitorConfig};
// 定义监控类型的 trait 后续的不同的监控类型都实现这个 trait
// 在这里添加 Send 和 Sync 作为父 trait
// Send: 允许在线程间移动
// Sync: 允许在线程间共享引用 (&T)
// 对于 tokio::spawn 和多线程异步，这两个通常都需要。
#[async_trait::async_trait]
pub trait Monitor: Send + Sync {
    // 引擎只负责产出内容（任务是否执行成功 + 具体结果详情），
    // 结果信封（id/monitor_type/target）由包装层统一组装
    async fn check(&self, config: &MonitorConfig) -> (bool, CheckResultDetail);
    // 引擎自述身份：类型字段的唯一事实来源
    fn get_type(&self) -> MonitorType;
}

// 声明具体的监控引擎
mod cpu;
mod disk;
mod dns;
mod ftp;
mod http;
mod icmp;
mod memory;
mod process;
mod tcp;
mod traceroute;
mod udp;
mod unknown;
// 不同类型的监控系统
use cpu::CpuMonitor;
use disk::DiskMonitor;
use dns::DnsMonitor;
use ftp::FtpMonitor;
use http::HttpMonitor;
use icmp::IcmpMonitor;
use memory::MemoryMonitor;
use process::ProcessMonitor;
use tcp::TcpMonitor;
use traceroute::TracerouteMonitor;
use udp::UdpMonitor;
use unknown::UnknownMonitor;

// 定义监控工厂
pub struct MonitorFactory;

impl MonitorFactory {
    // 基于策略模式 工厂函数返回一个实现特征Monitor的监控引擎
    // 入参为监控类型，出参为具体的监控引擎
    // 通过Box<>统一返回类型，同时因为尺寸未知必须要放在Box后面，同时通过vTable调用实现多态，调用端无需关注具体类型
    pub fn create_monitor(monitor_type: MonitorType) -> Box<dyn Monitor> {
        match monitor_type {
            MonitorType::Icmp => Box::new(IcmpMonitor::new()),
            MonitorType::Tcp => Box::new(TcpMonitor::new()),
            MonitorType::Udp => Box::new(UdpMonitor::new()),
            MonitorType::Dns => Box::new(DnsMonitor::new()),
            MonitorType::Http => Box::new(HttpMonitor::new()),
            MonitorType::Ftp => Box::new(FtpMonitor::new()),
            MonitorType::Traceroute => Box::new(TracerouteMonitor::new()),
            MonitorType::Cpu => Box::new(CpuMonitor::new()),
            MonitorType::Memory => Box::new(MemoryMonitor::new()),
            MonitorType::Disk => Box::new(DiskMonitor::new()),
            MonitorType::Process => Box::new(ProcessMonitor::new()),
            _ => Box::new(UnknownMonitor::new()),
        }
    }
}
