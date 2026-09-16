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
