use crate::database::models::{CheckResultModel, CheckResultModelInsert};
use crate::database::repositories::result_repo::CheckResultRepository;
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

    // 保存一次监控结果（数据持久化入口）
    pub fn save_check_result(&self, insert: &CheckResultModelInsert) -> Result<usize, Error> {
        self.repo.insert_check_result(insert)
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
