use crate::database::connect_db::{get_connection, SqlitePool};
use crate::database::models::{CheckResultModel, CheckResultModelInsert};
use crate::database::schema::check_result;
use diesel::prelude::*;
use std::sync::Arc;

pub struct CheckResultRepository {
    pool: Arc<SqlitePool>,
}

impl CheckResultRepository {
    pub fn new(pool: Arc<SqlitePool>) -> Self {
        CheckResultRepository { pool }
    }

    // 插入一条监控结果记录（数据持久化入口）
    pub fn insert_check_result(
        &self,
        insert: &CheckResultModelInsert,
    ) -> Result<usize, diesel::result::Error> {
        let mut conn = get_connection(&self.pool);
        diesel::insert_into(check_result::table)
            .values(insert)
            .execute(&mut conn)
    }

    // 删除 N 天前的监控结果（数据保留策略：check_result只增不删会无限膨胀）
    pub fn delete_older_than_days(&self, days: i64) -> Result<usize, diesel::result::Error> {
        let mut conn = get_connection(&self.pool);
        // created_at由SQLite的CURRENT_TIMESTAMP写入，是UTC时间，这里同样取UTC做比较
        let cutoff = (chrono::Utc::now() - chrono::Duration::days(days)).naive_utc();
        diesel::delete(check_result::table.filter(check_result::created_at.lt(Some(cutoff))))
            .execute(&mut conn)
    }

    // 批量插入多条监控结果：单条多行INSERT语句
    // （高监控数时每秒数百次独立小事务会加剧写锁竞争，合并后锁次数降为批次级）
    pub fn insert_check_results_batch(
        &self,
        inserts: &[CheckResultModelInsert],
    ) -> Result<usize, diesel::result::Error> {
        if inserts.is_empty() {
            return Ok(0);
        }
        let mut conn = get_connection(&self.pool);
        diesel::insert_into(check_result::table)
            .values(inserts)
            .execute(&mut conn)
    }

    // 每个监控的最新一条结果（/api/status 控制台聚合视图用）
    // 两段式类型安全查询：先取各monitor_id的最大结果id，再取回这些行
    pub fn get_latest_by_monitor(&self) -> Result<Vec<CheckResultModel>, diesel::result::Error> {
        let mut conn = get_connection(&self.pool);
        let max_ids: Vec<Option<i32>> = check_result::table
            .group_by(check_result::monitor_id)
            .select(diesel::dsl::max(check_result::id))
            .load(&mut conn)?;
        let ids: Vec<i32> = max_ids.into_iter().flatten().collect();
        check_result::table
            .filter(check_result::id.eq_any(ids))
            .load::<CheckResultModel>(&mut conn)
    }

    // 分页查询监控结果（monitor_id为None时查询全部）
    pub fn get_check_results(
        &self,
        monitor_id: Option<i32>,
        page: i64,
        page_size: i64,
    ) -> Result<(Vec<CheckResultModel>, i64), diesel::result::Error> {
        let mut conn = get_connection(&self.pool);
        let page_no = page.max(1);
        let page_sz = page_size.max(1);
        let offset = (page_no - 1) * page_sz;

        let mut count_query = check_result::table.into_boxed();
        let mut list_query = check_result::table.into_boxed();
        if let Some(mid) = monitor_id {
            count_query = count_query.filter(check_result::monitor_id.eq(mid));
            list_query = list_query.filter(check_result::monitor_id.eq(mid));
        }
        let total: i64 = count_query.count().get_result(&mut conn)?;
        let results = list_query
            .order(check_result::id.desc())
            .limit(page_sz)
            .offset(offset)
            .load::<CheckResultModel>(&mut conn)?;
        Ok((results, total))
    }
}

#[cfg(test)]
mod tests {
    use super::{CheckResultModelInsert, CheckResultRepository};
    use crate::database::connect_db::test_pool;
    use crate::database::repositories::test_support::create_test_monitor;
    use std::sync::Arc;

    #[test]
    fn result_insert_query_and_retention() {
        let dir = tempfile::tempdir().expect("临时目录创建失败");
        let pool = Arc::new(test_pool(dir.path()));
        let monitor_id = create_test_monitor(&pool, "t-result");
        let repo = CheckResultRepository::new(pool);
        let insert = CheckResultModelInsert {
            monitor_id,
            monitor_type: "HTTP".to_string(),
            status: 1,
            response_time: 12,
            metadata_json: Some(r#"{"check_id":"abc"}"#.to_string()),
        };
        repo.insert_check_result(&insert).unwrap();
        // 按monitor_id过滤分页查询
        let (list, total) = repo.get_check_results(Some(monitor_id), 1, 10).unwrap();
        assert_eq!(total, 1);
        assert_eq!(list[0].response_time, 12);
        assert_eq!(
            list[0].metadata_json.as_deref(),
            Some(r#"{"check_id":"abc"}"#)
        );
        // 保留策略：30天内的数据不会被清理
        assert_eq!(repo.delete_older_than_days(30).unwrap(), 0);
        // 负数天数表示截止时间在未来，全部清理
        assert_eq!(repo.delete_older_than_days(-1).unwrap(), 1);
        assert_eq!(repo.get_check_results(None, 1, 10).unwrap().1, 0);
    }

    #[test]
    fn batch_insert_roundtrip_and_empty_noop() {
        let dir = tempfile::tempdir().expect("临时目录创建失败");
        let pool = Arc::new(test_pool(dir.path()));
        let monitor_id = create_test_monitor(&pool, "t-batch");
        let repo = CheckResultRepository::new(pool);
        let inserts: Vec<CheckResultModelInsert> = (1..=3)
            .map(|i| CheckResultModelInsert {
                monitor_id,
                monitor_type: "HTTP".to_string(),
                status: 1,
                response_time: i * 10,
                metadata_json: None,
            })
            .collect();
        // 批量插入3条，一次调用全部落库
        assert_eq!(repo.insert_check_results_batch(&inserts).unwrap(), 3);
        let (list, total) = repo.get_check_results(Some(monitor_id), 1, 10).unwrap();
        assert_eq!(total, 3);
        assert_eq!(list[0].response_time, 30, "最新一条在前");
        assert_eq!(list[2].response_time, 10);
        // 空批次为no-op
        assert_eq!(repo.insert_check_results_batch(&[]).unwrap(), 0);
    }
}
