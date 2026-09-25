pub mod monitor_service;
pub mod result_service;

use crate::database::models::MonitorConfigInsert;
use crate::domain::SelfDefineMonitorConfig;

/// 把页面/JSON提交的监控配置项转换为数据库插入结构
/// name取target（无target的系统类监控取监控类型名），config_json保存完整原始配置用于还原
/// default_interval：interval未配置时写入DB列的缺省间隔（与调度器from_entry同源，保证列值=实际排程值）
pub fn build_monitor_insert(entry: &SelfDefineMonitorConfig, default_interval: u64) -> MonitorConfigInsert {
    let target = entry
        .target
        .clone()
        .unwrap_or_else(|| entry.monitor_type.to_string());
    // method序列化为字符串（如"GET"），未配置时为None
    let method = entry.method.as_ref().map(|m| {
        serde_json::to_string(m)
            .unwrap_or_default()
            .trim_matches('"')
            .to_string()
    });
    MonitorConfigInsert {
        name: Some(target.clone()),
        target,
        method,
        monitor_type: entry.monitor_type.to_string(),
        interval_ms: Some((entry.interval.unwrap_or(default_interval) * 1000) as i32),
        timeout_ms: entry.timeout.unwrap_or(5000) as i32,
        config_json: serde_json::to_string(entry).ok(),
        enabled: 1,
        tag: None,
    }
}
