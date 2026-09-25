use crate::database::models::{CheckResultModel, CheckResultModelInsert};
use crate::database::repositories::result_repo::CheckResultRepository;
use crate::monitor::types::CheckResult;
use diesel::result::Error;
use std::sync::Arc;

#[derive(Clone)]
pub struct ResultService {
    // 存储对Repo层对象的引用
    repo: Arc<CheckResultRepository>,
}
impl ResultService {
    pub fn new(repo: CheckResultRepository) -> Self {
        ResultService {
            repo: Arc::new(repo),
        }
    }

    // 从检查结果构造入库行：metadata_json = {"check_id": "<hex>", "details": {...}}
    // （check_id入库使CSV日志与数据库记录可关联；单条与批量持久化共用此唯一构造点）
    fn build_insert(monitor_id: i32, result: &CheckResult) -> CheckResultModelInsert {
        let (status, response_time, _status_code) = result.log_fields();
        let mut metadata = serde_json::Map::new();
        metadata.insert(
            "check_id".to_string(),
            serde_json::Value::String(format!("{:x}", result.id)),
        );
        if let Ok(details) = serde_json::to_value(&result.details) {
            metadata.insert("details".to_string(), details);
        }
        CheckResultModelInsert {
            monitor_id,
            monitor_type: result.monitor_type.to_string(),
            status: if status { 1 } else { 0 },
            response_time: response_time.min(i32::MAX as u128) as i32,
            metadata_json: serde_json::to_string(&serde_json::Value::Object(metadata)).ok(),
        }
    }

    // 领域写入口（单条）：手动执行等即时落库场景
    pub fn persist_check(&self, monitor_id: i32, result: &CheckResult) -> Result<usize, Error> {
        let insert = Self::build_insert(monitor_id, result);
        self.repo.insert_check_result(&insert)
    }

    // 领域写入口（批量）：消费者攒批场景，多行INSERT单语句落库
    pub fn persist_checks_batch(
        &self,
        results: &[(i32, &CheckResult)],
    ) -> Result<usize, Error> {
        let inserts: Vec<CheckResultModelInsert> = results
            .iter()
            .map(|(monitor_id, result)| Self::build_insert(*monitor_id, result))
            .collect();
        self.repo.insert_check_results_batch(&inserts)
    }

    // 分页查询监控结果（monitor_id为None时查询全部）
    pub fn get_check_results(
        &self,
        monitor_id: Option<i32>,
        page: i64,
        page_size: i64,
    ) -> Result<(Vec<CheckResultModel>, i64), Error> {
        self.repo.get_check_results(monitor_id, page, page_size)
    }

    // 游标（keyset）分页查询：翻页代价与页码无关，适合深翻页与大表。
    // before_id 为 None 取第一页（最新），否则取该 id 之前的一页
    pub fn get_check_results_keyset(
        &self,
        monitor_id: Option<i32>,
        before_id: Option<i32>,
        limit: i64,
    ) -> Result<Vec<CheckResultModel>, Error> {
        self.repo
            .get_check_results_keyset(monitor_id, before_id, limit)
    }

    // 每个监控的最新一条结果（/api/status聚合视图用；monitor_ids由调用方传入，
    // 固定两条查询消除 N+1、翻页代价与监控数无关——见repo注释）
    pub fn get_latest_by_monitor(
        &self,
        monitor_ids: &[i32],
    ) -> Result<Vec<CheckResultModel>, Error> {
        self.repo.get_latest_by_monitor(monitor_ids)
    }

    // 清理 N 天前的过期监控结果（数据保留策略）
    pub fn delete_expired(&self, days: i64) -> Result<usize, Error> {
        self.repo.delete_older_than_days(days)
    }
}

#[cfg(test)]
mod tests {
    use super::ResultService;
    use crate::database::pool::test_pool;
    use crate::database::repositories::result_repo::CheckResultRepository;
    use crate::database::repositories::test_support::create_test_monitor;
    use crate::monitor::types::{CheckResult, CheckResultDetail, IcmpMonitorResult};
    use crate::domain::MonitorType;
    use std::sync::Arc;

    // build_insert是单条与批量共用的唯一构造点：验证check_id与完整结果详情
    // 均进入metadata_json（详情入库后 /api/results 可直接回查失败现场）
    #[test]
    fn persist_check_roundtrips_check_id_and_details() {
        let dir = tempfile::tempdir().expect("临时目录创建失败");
        let pool = Arc::new(test_pool(dir.path()));
        let monitor_id = create_test_monitor(&pool, "t-details");
        let service = ResultService::new(CheckResultRepository::new(pool));

        let result = CheckResult {
            id: 0xdeadbeef,
            monitor_type: MonitorType::Icmp,
            target: Some("10.0.0.1".to_string()),
            status: true,
            details: CheckResultDetail::Icmp(IcmpMonitorResult {
                is_alive: true,
                elapsed_ms: 42,
                rtt_ms: None,
            }),
        };
        service.persist_check(monitor_id, &result).unwrap();

        let (list, total) = service.get_check_results(Some(monitor_id), 1, 10).unwrap();
        assert_eq!(total, 1);
        let metadata = list[0].metadata_json.as_deref().expect("metadata_json应存在");
        let v: serde_json::Value = serde_json::from_str(metadata).expect("metadata_json应为合法JSON");
        // check_id为16进制（与CSV日志关联）
        assert_eq!(v["check_id"], "deadbeef");
        // details为序列化的CheckResultDetail（外标签枚举：{"Icmp":{...}}），完整可回查
        let details = v["details"].as_object().expect("details应为JSON对象");
        assert!(details.contains_key("Icmp"), "details应保留类型标签: {details:?}");
        assert_eq!(details["Icmp"]["elapsed_ms"], 42);
    }

    // get_latest_by_monitor 是对 repo 同名方法的一行透传，latest-per-monitor 语义
    // （每监控取最新一条 / 无记录跳过 / 空输入返回空）已由
    // result_repo.rs::latest_by_monitor_returns_one_newest_per_monitor 完整覆盖，
    // 此处不再重复钉查询行为
}
