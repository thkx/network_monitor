pub mod monitor_repo;
pub mod result_repo;
pub mod alert_state_repo;

// 测试公共设施：建一条监控父行
// （check_result/alert_state 的 monitor_id 外键指向 monitor_config，测试需先建父行）
#[cfg(test)]
pub(crate) mod test_support {
    use crate::database::pool::SqlitePool;
    use crate::database::models::MonitorConfigInsert;
    use crate::database::repositories::monitor_repo::MonitorRepository;
    use std::sync::Arc;

    pub(crate) fn create_test_monitor(pool: &Arc<SqlitePool>, name: &str) -> i32 {
        MonitorRepository::new(pool.clone())
            .create_monitor(&MonitorConfigInsert {
                name: Some(name.to_string()),
                target: "https://a.com".to_string(),
                method: Some("GET".to_string()),
                monitor_type: "HTTP".to_string(),
                interval_ms: Some(5000),
                timeout_ms: 5000,
                config_json: Some("{}".to_string()),
                enabled: 1,
                tag: None,
            })
            .expect("测试监控创建失败")
            .id
    }
}
