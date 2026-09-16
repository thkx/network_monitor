use super::monitor_trait::Monitor;
use super::types::{
    CheckResultDetail, MonitorConfig, ProcessBrief, ProcessMonitorResult,
};
use crate::tools_types::MonitorType;
use sysinfo::System;

pub struct ProcessMonitor {}

impl ProcessMonitor {
    pub fn new() -> Self {
        ProcessMonitor {}
    }
}

// 用来让trait中的异步方法可用，详细的请查看Q&A
#[async_trait::async_trait]
impl Monitor for ProcessMonitor {
    async fn check(&self, _config: &MonitorConfig) -> (bool, CheckResultDetail) {
        let mut sys = System::new();
        // 刷新进程列表信息
        sys.refresh_processes();
        // 系统进程总数
        let process_count = sys.processes().len();
        // 收集所有进程的简要信息
        let mut top_by_memory: Vec<ProcessBrief> = sys
            .processes()
            .iter()
            .map(|(pid, process)| ProcessBrief {
                pid: pid.as_u32(),
                name: process.name().to_string(),
                memory_bytes: process.memory(),
            })
            .collect();
        // 按内存占用降序排序，取前10个
        top_by_memory.sort_by_key(|p| std::cmp::Reverse(p.memory_bytes));
        top_by_memory.truncate(10);
        (
            true,
            CheckResultDetail::Process(ProcessMonitorResult {
                process_count,
                top_by_memory,
            }),
        )
    }

    fn get_type(&self) -> MonitorType {
        MonitorType::Process
    }
}
