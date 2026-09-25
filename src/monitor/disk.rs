use super::Monitor;
use crate::domain::MonitorType;
use crate::domain::{CheckResultDetail, DiskInfo, DiskMonitorResult, MonitorConfig};
use sysinfo::Disks;

pub struct DiskMonitor {}

impl DiskMonitor {
    pub fn new() -> Self {
        DiskMonitor {}
    }
}

// 用来让trait中的异步方法可用，详细的请查看Q&A
#[async_trait::async_trait]
impl Monitor for DiskMonitor {
    async fn check(&self, _config: &MonitorConfig) -> (bool, CheckResultDetail) {
        // 获取所有磁盘的列表并刷新信息
        let disks = Disks::new_with_refreshed_list();
        let mut total_bytes = 0u64;
        let mut available_bytes = 0u64;
        let mut disk_infos: Vec<DiskInfo> = vec![];
        for disk in disks.list() {
            let d_total = disk.total_space();
            let d_available = disk.available_space();
            total_bytes += d_total;
            available_bytes += d_available;
            disk_infos.push(DiskInfo {
                name: disk.name().to_string_lossy().to_string(),
                mount_point: disk.mount_point().to_string_lossy().to_string(),
                total_bytes: d_total,
                available_bytes: d_available,
            });
        }
        (
            true,
            CheckResultDetail::Disk(DiskMonitorResult {
                total_bytes,
                available_bytes,
                disks: disk_infos,
            }),
        )
    }

    fn get_type(&self) -> MonitorType {
        MonitorType::Disk
    }
}
