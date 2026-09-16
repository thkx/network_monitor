// src/monitor/monitor_trait.rs
use super::types::{CheckResultDetail, MonitorConfig};
use crate::tools_types::MonitorType;
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
