use super::Monitor;
use super::types::{
    CheckResultDetail, MonitorConfig, ProcessBrief, ProcessMonitorResult,
};
use crate::domain::MonitorType;
use std::sync::Mutex;
use sysinfo::System;

pub struct ProcessMonitor {
    // 跨轮次复用同一 System：refresh_processes 会增量更新进程表
    // （复用可保留上轮进程条目，只增删变化项），比每轮全量重建更省。
    // check 取 &self，用 Mutex 提供内部可变性；同一任务串行调用无锁竞争
    sys: Mutex<System>,
}

impl ProcessMonitor {
    pub fn new() -> Self {
        ProcessMonitor {
            sys: Mutex::new(System::new()),
        }
    }
}

// 用来让trait中的异步方法可用，详细的请查看Q&A
#[async_trait::async_trait]
impl Monitor for ProcessMonitor {
    async fn check(&self, _config: &MonitorConfig) -> (bool, CheckResultDetail) {
        let mut sys = self.sys.lock().unwrap();
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
