use crate::database::pool::{get_connection, SqlitePool};
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
    // 两次查询消除 N+1：此前按 monitor_id 逐个 SELECT ... LIMIT 1，N 个监控 N 次往返，
    // 而本端点是登录控制台每5秒轮询的最高频DB调用——监控数上百时每轮上百次往返，
    // 连接池争用与调度开销都随监控数线性放大。
    // 改为固定两条查询（与监控数无关）：
    //   1) GROUP BY monitor_id 取 max(id)，一次拿到各监控最新一条的主键
    //   2) id IN (那批主键) 一次性把行取回
    // id 为 rowid 别名、随插入单调递增，max(id) 即该监控最新一条，与旧 ORDER BY
    // created_at DESC LIMIT 1 语义等价（同秒并列也由 id 序决出最新）。
    // 两步均走已有索引（idx_check_result_monitor_created / 主键），无全表扫描
    pub fn get_latest_by_monitor(
        &self,
        monitor_ids: &[i32],
    ) -> Result<Vec<CheckResultModel>, diesel::result::Error> {
        if monitor_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut conn = get_connection(&self.pool)?;
        // 第1步：各监控最新一条的 id（GROUP BY 聚合，一次往返）
        let latest_ids: Vec<Option<i32>> = check_result::table
            .filter(check_result::monitor_id.eq_any(monitor_ids))
            .group_by(check_result::monitor_id)
            .select(diesel::dsl::max(check_result::id))
            .load(&mut conn)?;
        let ids: Vec<i32> = latest_ids.into_iter().flatten().collect();
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        // 第2步：按主键集合一次性取回行
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
    use crate::database::pool::test_pool;
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

    // get_latest_by_monitor 两步查询：每监控恰好一条且取最新，空输入/无记录不报错
    #[test]
    fn latest_by_monitor_returns_one_newest_per_monitor() {
        let dir = tempfile::tempdir().expect("临时目录创建失败");
        let pool = Arc::new(test_pool(dir.path()));
        let a = create_test_monitor(&pool, "t-latest-a");
        let b = create_test_monitor(&pool, "t-latest-b");
        let c = create_test_monitor(&pool, "t-latest-c"); // 无任何结果，验证不报错也不返回
        let repo = CheckResultRepository::new(pool);
        // a 插 3 条（response_time 10/20/30，最新是30）；b 插 1 条
        for rt in [10, 20, 30] {
            repo.insert_check_result(&CheckResultModelInsert {
                monitor_id: a,
                monitor_type: "HTTP".to_string(),
                status: if rt == 30 { 0 } else { 1 },
                response_time: rt,
                metadata_json: None,
            })
            .unwrap();
        }
        repo.insert_check_result(&CheckResultModelInsert {
            monitor_id: b,
            monitor_type: "HTTP".to_string(),
            status: 1,
            response_time: 77,
            metadata_json: None,
        })
        .unwrap();

        let latest = repo.get_latest_by_monitor(&[a, b, c]).unwrap();
        assert_eq!(latest.len(), 2, "a、b 各一条，c 无记录被跳过");
        let row_a = latest.iter().find(|r| r.monitor_id == a).expect("应含a");
        assert_eq!(row_a.response_time, 30, "a 应取最新（response_time=30）");
        assert_eq!(row_a.status, 0, "最新那条 status=0，证明取的是最新非最旧");
        let row_b = latest.iter().find(|r| r.monitor_id == b).expect("应含b");
        assert_eq!(row_b.response_time, 77);

        // 空输入：直接返回空，不触库
        assert!(repo.get_latest_by_monitor(&[]).unwrap().is_empty());
        // 全部无记录：返回空
        assert!(repo.get_latest_by_monitor(&[c]).unwrap().is_empty());
    }
}
