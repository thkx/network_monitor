
/// 默认的重试策略：200ms起步、5s封顶、最多重试3次（供数据库连接等场景复用）
pub fn default_retry_policy() -> backon::ExponentialBuilder {
    backon::ExponentialBuilder::default()
        .with_min_delay(std::time::Duration::from_millis(200))
        .with_max_delay(std::time::Duration::from_secs(5))
        .with_max_times(3)
}