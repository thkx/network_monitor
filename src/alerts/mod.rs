// 告警模块组织：
//   engine —— 告警状态机（规则评估、防抖、抑制状态持久化）与消息文本构造
//   notify —— 通知渠道适配（飞书/钉钉/企业微信、签名、后台退避重试）
// 两者变化原因不同：告警语义改动不碰渠道代码，渠道增减不碰状态机
mod engine;
mod notify;

pub use engine::AlertsEngine;

/// 恢复通知消息前缀：engine构造恢复消息与notify EMAIL主题判别的共享约定。
/// 常量化防止两处字符串字面量各自漂移（主题分类会静默错位）
pub(crate) const RECOVERY_PREFIX: &str = "[监控恢复]";
