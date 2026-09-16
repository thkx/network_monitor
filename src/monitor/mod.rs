pub mod types;
use crate::tools_types::MonitorType; // 全局通用的类型定义模块

// 引进特征对象
pub mod monitor_trait;
pub use monitor_trait::Monitor;

// 声明具体的监控引擎
mod cpu_monitor;
mod disk_monitor;
mod dns_monitor;
mod ftp_monitor;
mod http_monitor;
mod icmp_monitor;
mod memory_monitor;
mod process_monitor;
mod tcp_monitor;
mod traceroute_monitor;
mod udp_monitor;
mod unknown_monitor;
// 不同类型的监控系统
use cpu_monitor::CpuMonitor;
use disk_monitor::DiskMonitor;
use dns_monitor::DnsMonitor;
use ftp_monitor::FtpMonitor;
use http_monitor::HttpMonitor;
use icmp_monitor::IcmpMonitor;
use memory_monitor::MemoryMonitor;
use process_monitor::ProcessMonitor;
use tcp_monitor::TcpMonitor;
use traceroute_monitor::TracerouteMonitor;
use udp_monitor::UdpMonitor;
use unknown_monitor::UnknownMonitor;

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
