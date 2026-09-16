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

    // 每个监控的最新一条结果（/api/status 控制台聚合视图用）
    pub fn get_latest_by_monitor(&self) -> Result<Vec<CheckResultModel>, Error> {
        self.repo.get_latest_by_monitor()
    }

    // 清理 N 天前的过期监控结果（数据保留策略）
    pub fn delete_expired(&self, days: i64) -> Result<usize, Error> {
        self.repo.delete_older_than_days(days)
    }
}
