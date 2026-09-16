// 告警模块组织：
//   engine —— 告警状态机（规则评估、防抖、抑制状态持久化）与消息文本构造
//   notify —— 通知渠道适配（飞书/钉钉/企业微信、签名、后台退避重试）
// 两者变化原因不同：告警语义改动不碰渠道代码，渠道增减不碰状态机
mod engine;
mod notify;

pub use engine::AlertsEngine;
