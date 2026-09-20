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
        let mut conn = get_connection(&self.pool)?;
        diesel::insert_into(check_result::table)
            .values(insert)
            .execute(&mut conn)
    }

    // 删除 N 天前的监控结果（数据保留策略：check_result只增不删会无限膨胀）
    pub fn delete_older_than_days(&self, days: i64) -> Result<usize, diesel::result::Error> {
        let mut conn = get_connection(&self.pool)?;
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
        let mut conn = get_connection(&self.pool)?;
        diesel::insert_into(check_result::table)
            .values(inserts)
            .execute(&mut conn)
    }

    // 每个监控的最新一条结果（/api/status 控制台聚合视图用）
    // 逐监控索引化查询：WHERE monitor_id = ? ORDER BY created_at DESC LIMIT 1
    // 走 (monitor_id, created_at) 复合索引反向扫描，O(log n)/监控。
    // 此前 GROUP BY monitor_id + max(id) 形态需全表扫描，而本查询挂在登录控制台
    // 每5秒轮询的/api/status上——check_result保留期内可达百万行，不可接受。
    // 同一created_at秒内的并列由索引内rowid序（id为rowid别名）反向保证取最新id，
    // 与旧max(id)语义等价
    pub fn get_latest_by_monitor(
        &self,
        monitor_ids: &[i32],
    ) -> Result<Vec<CheckResultModel>, diesel::result::Error> {
        use diesel::OptionalExtension;
        let mut conn = get_connection(&self.pool)?;
        let mut latest = Vec::with_capacity(monitor_ids.len());
        for mid in monitor_ids {
            if let Some(row) = check_result::table
                .filter(check_result::monitor_id.eq(*mid))
                .order_by(check_result::created_at.desc())
                .first::<CheckResultModel>(&mut conn)
                .optional()?
            {
                latest.push(row);
            }
        }
        Ok(latest)
    }

    // 分页查询监控结果（monitor_id为None时查询全部）
    pub fn get_check_results(
        &self,
        monitor_id: Option<i32>,
        page: i64,
        page_size: i64,
    ) -> Result<(Vec<CheckResultModel>, i64), diesel::result::Error> {
        let mut conn = get_connection(&self.pool)?;
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

    // 游标（keyset）分页：按 id 降序取 before_id 之前的一页。
    // 与 OFFSET 分页的区别——OFFSET 需扫描并丢弃前 N 行，翻到深页（page_no 很大）时
    // 在百万行的 check_result 上会退化为近全表扫描；keyset 用 `id < before_id` 直接命中
    // 主键索引定位，翻页代价与页码无关（O(log n + page_size)）。
    // before_id 为 None 表示取第一页（最新）；返回结果按 id 降序，调用方用最后一条的 id 作为
    // 下一页的游标。不返回 total（keyset 语义下总数与翻页解耦，需要时另查 count）
    pub fn get_check_results_keyset(
        &self,
        monitor_id: Option<i32>,
        before_id: Option<i32>,
        limit: i64,
    ) -> Result<Vec<CheckResultModel>, diesel::result::Error> {
        let mut conn = get_connection(&self.pool)?;
        let lim = limit.clamp(1, 500);
        let mut query = check_result::table.into_boxed();
        if let Some(mid) = monitor_id {
            query = query.filter(check_result::monitor_id.eq(mid));
        }
        if let Some(cursor) = before_id {
            query = query.filter(check_result::id.lt(cursor));
        }
        query
            .order(check_result::id.desc())
            .limit(lim)
            .load::<CheckResultModel>(&mut conn)
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

    // keyset游标分页：按id降序、游标之前取页，跨页无重叠无遗漏、按monitor_id隔离
    #[test]
    fn keyset_pagination_walks_pages_without_overlap() {
        let dir = tempfile::tempdir().expect("临时目录创建失败");
        let pool = Arc::new(test_pool(dir.path()));
        let monitor_id = create_test_monitor(&pool, "t-keyset");
        let other_id = create_test_monitor(&pool, "t-keyset-other");
        let repo = CheckResultRepository::new(pool);
        // 目标监控插5条，另一监控插2条（验证过滤隔离）
        for i in 1..=5 {
            repo.insert_check_result(&CheckResultModelInsert {
                monitor_id,
                monitor_type: "HTTP".to_string(),
                status: 1,
                response_time: i * 10,
                metadata_json: None,
            })
            .unwrap();
        }
        for _ in 0..2 {
            repo.insert_check_result(&CheckResultModelInsert {
                monitor_id: other_id,
                monitor_type: "HTTP".to_string(),
                status: 1,
                response_time: 999,
                metadata_json: None,
            })
            .unwrap();
        }

        // 第一页（before_id=None）：取最新2条，id降序
        let page1 = repo
            .get_check_results_keyset(Some(monitor_id), None, 2)
            .unwrap();
        assert_eq!(page1.len(), 2);
        assert!(page1[0].id > page1[1].id, "应按id降序");
        assert_eq!(page1[0].response_time, 50, "最新一条在前");

        // 第二页：游标=第一页末条id，取其之前2条，与第一页无重叠
        let cursor = page1.last().unwrap().id;
        let page2 = repo
            .get_check_results_keyset(Some(monitor_id), Some(cursor), 2)
            .unwrap();
        assert_eq!(page2.len(), 2);
        assert!(page2[0].id < cursor, "第二页应严格在游标之前");
        assert!(
            page2.iter().all(|r| r.id < page1[1].id),
            "跨页无重叠"
        );

        // 第三页：剩1条
        let cursor2 = page2.last().unwrap().id;
        let page3 = repo
            .get_check_results_keyset(Some(monitor_id), Some(cursor2), 2)
            .unwrap();
        assert_eq!(page3.len(), 1, "第五条落在最后一页");
        assert_eq!(page3[0].response_time, 10, "最旧一条");

        // 走到底：游标过末条后返回空
        let cursor3 = page3.last().unwrap().id;
        let page4 = repo
            .get_check_results_keyset(Some(monitor_id), Some(cursor3), 2)
            .unwrap();
        assert!(page4.is_empty(), "走到底应返回空页");

        // monitor_id过滤隔离：目标监控总计5条，另一监控的记录不混入
        let all_target = repo
            .get_check_results_keyset(Some(monitor_id), None, 500)
            .unwrap();
        assert_eq!(all_target.len(), 5, "只返回目标监控的记录");
    }
}
