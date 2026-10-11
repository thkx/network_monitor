use super::Monitor;
use crate::domain::MonitorType;
use crate::domain::{CheckResultDetail, DnsMonitorResult, MonitorConfig};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::OnceCell;
use trust_dns_resolver::TokioAsyncResolver;

pub struct DnsMonitor {
    // 跨轮次复用同一 resolver：tokio_from_system_conf 每次要读系统配置并初始化线程池
    // （Windows 上实测可达秒级），而 resolver 句柄本身可共享——此前每轮 check 重建纯属浪费，
    // 还会挤占 config.timeout 的解析预算。check 取 &self（trait 契约），用 OnceCell 懒初始化。
    resolver: OnceCell<Arc<TokioAsyncResolver>>,
}

impl DnsMonitor {
    pub fn new() -> Self {
        DnsMonitor {
            resolver: OnceCell::new(),
        }
    }

    // 懒初始化并复用系统 resolver：初始化失败时不缓存失败态（下轮可重试），返回 None。
    async fn resolver(&self) -> Option<Arc<TokioAsyncResolver>> {
        self.resolver
            .get_or_try_init(|| async {
                TokioAsyncResolver::tokio_from_system_conf().map(Arc::new)
            })
            .await
            .ok()
            .cloned()
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
        // 整体解析超时对齐 config.timeout（毫秒；下限1秒防配置0立即超时）。
        // 此前无兜底超时，依赖系统解析器默认值——配置的超时对DNS不生效，
        // 且异常时可能长时间挂起。用外层timeout给一个硬上界。
        let deadline = std::time::Duration::from_millis(config.timeout.max(1000));
        // 复用懒初始化的系统解析器；创建失败时本次检查判为失败，不 panic
        let Some(resolver) = self.resolver().await else {
            tracing::warn!("DNS resolver 初始化失败，本次检查判为失败");
            return (
                false,
                CheckResultDetail::Dns(DnsMonitorResult {
                    resolved: false,
                    elapsed_ms: start.elapsed().as_millis(),
                    ips: vec![],
                }),
            );
        };
        // 查询目标域名的A记录
        if let Ok(Ok(lookup)) =
            tokio::time::timeout(deadline, resolver.lookup_ip(target.as_str())).await
        {
            resolved = true;
            ips = lookup.iter().map(|ip| ip.to_string()).collect();
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
