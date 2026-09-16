use super::monitor_trait::Monitor;
use super::types::{CheckResultDetail, CpuMonitorResult, MonitorConfig};
use crate::tools_types::MonitorType;
use sysinfo::{System, MINIMUM_CPU_UPDATE_INTERVAL};

pub struct CpuMonitor {}

impl CpuMonitor {
    pub fn new() -> Self {
        CpuMonitor {}
    }
}

// 用来让trait中的异步方法可用，详细的请查看Q&A
#[async_trait::async_trait]
impl Monitor for CpuMonitor {
    async fn check(&self, _config: &MonitorConfig) -> (bool, CheckResultDetail) {
        let mut sys = System::new();
        // 第一次刷新只做数据初始化，CPU使用率需要间隔一段时间后再次刷新才能拿到真实值
        sys.refresh_cpu_usage();
        tokio::time::sleep(MINIMUM_CPU_UPDATE_INTERVAL).await;
        sys.refresh_cpu_usage();
        // 总体CPU使用率
        let usage_percent = sys.global_cpu_info().cpu_usage();
        // CPU逻辑核心数
        let core_count = sys.cpus().len();
        (
            true,
            CheckResultDetail::Cpu(CpuMonitorResult {
                usage_percent,
                core_count,
            }),
        )
    }

    fn get_type(&self) -> MonitorType {
        MonitorType::Cpu
    }
}
