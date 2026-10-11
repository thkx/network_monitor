use super::Monitor;
use crate::domain::MonitorType;
use crate::domain::{CheckResultDetail, DiskInfo, DiskMonitorResult, MonitorConfig};
use std::sync::Mutex;
use sysinfo::Disks;

pub struct DiskMonitor {
    // 跨轮次复用同一 Disks：此前每轮 check 重建（Disks::new_with_refreshed_list 要重新枚举
    // 所有磁盘），改为常驻后每轮仅做增量刷新。check 取 &self（trait 契约），用 Mutex 提供
    // 内部可变性；单个监控任务串行调用同一实例，锁无竞争。
    disks: Mutex<Disks>,
}

impl DiskMonitor {
    pub fn new() -> Self {
        DiskMonitor {
            disks: Mutex::new(Disks::new_with_refreshed_list()),
        }
    }
}

// 用来让trait中的异步方法可用，详细的请查看Q&A
#[async_trait::async_trait]
impl Monitor for DiskMonitor {
    async fn check(&self, _config: &MonitorConfig) -> (bool, CheckResultDetail) {
        let mut total_bytes = 0u64;
        let mut available_bytes = 0u64;
        let mut disk_infos: Vec<DiskInfo> = vec![];
        // 对常驻 Disks 做增量刷新：refresh_list 同步新增/移除的磁盘，refresh 更新各盘空间数据。
        // 本块无 await，锁在作用域结束（drop(disks)）前即释放，不会跨 await 点持有 std::Mutex。
        {
            let mut disks = self.disks.lock().unwrap();
            disks.refresh_list();
            disks.refresh();
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
