use super::monitor_trait::Monitor;
use super::types::{CheckResultDetail, MemoryMonitorResult, MonitorConfig};
use crate::tools_types::MonitorType;
use sysinfo::System;

pub struct MemoryMonitor {}

impl MemoryMonitor {
    pub fn new() -> Self {
        MemoryMonitor {}
    }
}

// 用来让trait中的异步方法可用，详细的请查看Q&A
#[async_trait::async_trait]
impl Monitor for MemoryMonitor {
    async fn check(&self, _config: &MonitorConfig) -> (bool, CheckResultDetail) {
        let mut sys = System::new();
        // 刷新内存信息
        sys.refresh_memory();
        // 总内存（字节）
        let total_bytes = sys.total_memory();
        // 已使用内存（字节）
        let used_bytes = sys.used_memory();
        // 计算内存使用率
        let usage_percent = if total_bytes > 0 {
            used_bytes as f32 / total_bytes as f32 * 100.0
        } else {
            0.0
        };
        (
            true,
            CheckResultDetail::Memory(MemoryMonitorResult {
                total_bytes,
                used_bytes,
                usage_percent,
            }),
        )
    }

    fn get_type(&self) -> MonitorType {
        MonitorType::Memory
    }
}
