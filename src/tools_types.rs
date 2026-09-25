// 全局类型定义门面：按关注点拆分到三个子模块，此处统一 re-export，
// 保持 `crate::tools_types::X` 的既有引用路径不变（拆分对调用方透明）。
//   - result: HTTP 探测产出结果结构（输出侧）
//   - config: 监控输入配置、监控类型、HTTP 请求相关类型
//   - alert:  告警/通知配置、渠道类型（NotifyType）与阈值比较符（CompareOp）
mod alert;
mod config;
mod result;

pub use alert::*;
pub use config::*;
pub use result::*;
