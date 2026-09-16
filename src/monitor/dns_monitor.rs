use super::monitor_trait::Monitor;
use super::types::{CheckResultDetail, DnsMonitorResult, MonitorConfig};
use crate::tools_types::MonitorType;
use std::time::Instant;
use trust_dns_resolver::TokioAsyncResolver;

pub struct DnsMonitor {}

impl DnsMonitor {
    pub fn new() -> Self {
        DnsMonitor {}
    }
}

// 用来让trait中的异步方法可用，详细的请查看Q&A
#[async_trait::async_trait]
impl Monitor for DnsMonitor {
    async fn check(&self, config: &MonitorConfig) -> (bool, CheckResultDetail) {
        // 判断一下是否存在 target 此时的target 应该是一个域名
        if config.target.is_none() {
            return (false, CheckResultDetail::Dns(DnsMonitorResult::default()));
        }
        let target = config.target.as_ref().unwrap();
        let start = Instant::now();
        let mut resolved = false; // 是否解析成功
        let mut ips: Vec<String> = vec![]; // 解析到的IP列表
        // 使用系统DNS配置创建异步解析器
        if let Ok(resolver) = TokioAsyncResolver::tokio_from_system_conf() {
            // 查询目标域名的A记录
            if let Ok(lookup) = resolver.lookup_ip(target.as_str()).await {
                resolved = true;
                ips = lookup.iter().map(|ip| ip.to_string()).collect();
            }
        }
        let elapsed_ms = start.elapsed().as_millis();
        (
            true,
            CheckResultDetail::Dns(DnsMonitorResult {
                resolved,
                elapsed_ms,
                ips,
            }),
        )
    }

    fn get_type(&self) -> MonitorType {
        MonitorType::Dns
    }
}
