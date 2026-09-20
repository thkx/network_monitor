use super::monitor_trait::Monitor;
use super::types::{CheckResultDetail, CpuMonitorResult, MonitorConfig};
use crate::tools_types::MonitorType;
use std::sync::Mutex;
use sysinfo::{System, MINIMUM_CPU_UPDATE_INTERVAL};

pub struct CpuMonitor {
    // 跨轮次复用同一 System：避免每次 check 重建（重新探测 CPU 拓扑），
    // 且上一轮的刷新点即本轮的采样基线——省去每轮 ~200ms 的预热等待，
    // 得到的是"两次 check 间隔内"的平均使用率（对监控更有意义）。
    // check 取 &self（trait 契约），故用 Mutex 提供内部可变性；
    // 单个监控任务串行调用同一实例，锁无竞争
    sys: Mutex<System>,
    // 是否已建立基线：首轮（含每次 run_once 新建实例）无历史采样点，
    // 需按 sysinfo 要求做一次预热（刷新→等待→再刷新）才能得到有效使用率
    warmed: Mutex<bool>,
}

impl CpuMonitor {
    pub fn new() -> Self {
        CpuMonitor {
            sys: Mutex::new(System::new()),
            warmed: Mutex::new(false),
        }
    }
}

// 用来让trait中的异步方法可用，详细的请查看Q&A
#[async_trait::async_trait]
impl Monitor for CpuMonitor {
    async fn check(&self, _config: &MonitorConfig) -> (bool, CheckResultDetail) {
        // 首轮需预热：刷新一次建立基线，等待最小间隔后再刷新。
        // sleep 前必须释放锁（不跨 await 持有 std::Mutex）；同一任务串行调用，
        // 释放期间不会有并发 check 介入
        let need_warmup = {
            let mut warmed = self.warmed.lock().unwrap();
            if *warmed {
                false
            } else {
                *warmed = true;
                true
            }
        };
        if need_warmup {
            self.sys.lock().unwrap().refresh_cpu_usage();
            tokio::time::sleep(MINIMUM_CPU_UPDATE_INTERVAL).await;
        }
        // 非首轮：直接刷新，用上一轮的采样点作基线（间隔即两次 check 之间）
        let mut sys = self.sys.lock().unwrap();
        sys.refresh_cpu_usage();
        let usage_percent = sys.global_cpu_info().cpu_usage();
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

#[cfg(test)]
mod tests {
    use super::{CpuMonitor, Monitor, MINIMUM_CPU_UPDATE_INTERVAL};
    use crate::monitor::types::{CheckResultDetail, MonitorConfig, MonitorConfigDetail};
    use crate::tools_types::MonitorType;

    fn cpu_config() -> MonitorConfig {
        MonitorConfig {
            target: None,
            interval: Some(60),
            monitor_type: MonitorType::Cpu,
            timeout: 5000,
            details: MonitorConfigDetail::Cpu(crate::monitor::types::CpuMonitorConfig {}),
        }
    }

    // 首轮预热耗时 ≥ 最小采样间隔；复用实例后的第二轮跳过预热，明显更快。
    // 同时验证两轮都产出有效结果（核数>0、使用率非负）
    #[tokio::test]
    async fn reused_instance_skips_warmup_after_first_check() {
        let monitor = CpuMonitor::new();
        let cfg = cpu_config();

        let t0 = std::time::Instant::now();
        let (ok1, d1) = monitor.check(&cfg).await;
        let first = t0.elapsed();
        assert!(ok1);

        let t1 = std::time::Instant::now();
        let (ok2, d2) = monitor.check(&cfg).await;
        let second = t1.elapsed();
        assert!(ok2);

        // 首轮含预热等待，第二轮不含：第二轮应远快于最小采样间隔
        assert!(
            first >= MINIMUM_CPU_UPDATE_INTERVAL,
            "首轮应包含预热等待: {first:?}"
        );
        assert!(
            second < MINIMUM_CPU_UPDATE_INTERVAL,
            "复用实例第二轮应跳过预热: {second:?}"
        );

        for d in [d1, d2] {
            match d {
                CheckResultDetail::Cpu(r) => {
                    assert!(r.core_count > 0, "核数应>0");
                    assert!(r.usage_percent >= 0.0, "使用率非负");
                }
                other => panic!("应为CPU结果: {other:?}"),
            }
        }
    }
}
