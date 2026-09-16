use super::monitor_trait::Monitor;
use super::types::{CheckResultDetail, MonitorConfig, MonitorConfigDetail, UnknownMonitorResult};
use crate::tools_types::MonitorType;

pub struct UnknownMonitor {}

impl UnknownMonitor {
    pub fn new() -> Self {
        UnknownMonitor {}
    }
}

// 用来让trait中的异步方法可用，详细的请查看Q&A
#[async_trait::async_trait]
impl Monitor for UnknownMonitor {
    async fn check(&self, config: &MonitorConfig) -> (bool, CheckResultDetail) {
        // 读取配置侧的兜底描述信息并透传进结果：
        // 正常路径下配置详情就是Unknown，直接复用其description；
        // 若配置详情与监控引擎不匹配（异常路径），给出明确的排查提示
        let description = match &config.details {
            MonitorConfigDetail::Unknown(u) => format!(
                "{}（monitor_type: {}）",
                u.description, config.monitor_type
            ),
            _ => "监控类型与配置详情不匹配，请检查配置中的monitor_type".to_string(),
        };
        (
            false,
            CheckResultDetail::Unknown(UnknownMonitorResult {
                description,
                query_type: config.monitor_type,
            }),
        )
    }

    fn get_type(&self) -> MonitorType {
        MonitorType::Unknown
    }
}
