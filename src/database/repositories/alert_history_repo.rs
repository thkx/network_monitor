use crate::database::models::{AlertHistoryInsert, AlertHistoryModel};
use crate::database::pool::{SqlitePool, get_connection};
use crate::database::schema::alert_history;
use diesel::prelude::*;
use std::sync::Arc;

// 告警历史仓库：写入"触发/恢复"事件、按 keyset 分页读取
// 与 AlertStateRepository 一样由告警引擎直接使用；Clone 仅复制 Arc 句柄，
// 供 record_history 挪入 spawn_blocking 使用
#[derive(Clone)]
pub struct AlertHistoryRepository {
    pool: Arc<SqlitePool>,
}

impl AlertHistoryRepository {
    pub fn new(pool: Arc<SqlitePool>) -> Self {
        AlertHistoryRepository { pool }
    }

    // 追加一条告警历史
    pub fn insert(&self, rec: &AlertHistoryInsert) -> Result<usize, diesel::result::Error> {
        let mut conn = get_connection(&self.pool)?;
        diesel::insert_into(alert_history::table)
            .values(rec)
            .execute(&mut conn)
    }

    // keyset（游标）分页：按 id 降序取 before_id 之前的一页（monitor_id 可选过滤）。
    // before_id 为 None 取第一页（最新）；返回按 id 降序，调用方以末条 id 作为下一页游标。
    // 与 check_result 的 keyset 同源：翻页代价与页码无关，走 (monitor_id, id) 索引
    pub fn get_history(
        &self,
        monitor_id: Option<i32>,
        before_id: Option<i32>,
        limit: i64,
    ) -> Result<Vec<AlertHistoryModel>, diesel::result::Error> {
        let mut conn = get_connection(&self.pool)?;
        let lim = limit.clamp(1, 500);
        let mut query = alert_history::table.into_boxed();
        if let Some(mid) = monitor_id {
            query = query.filter(alert_history::monitor_id.eq(mid));
        }
        if let Some(cursor) = before_id {
            query = query.filter(alert_history::id.lt(cursor));
        }
        query
            .order(alert_history::id.desc())
            .limit(lim)
            .load::<AlertHistoryModel>(&mut conn)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::pool::test_pool;
    use crate::database::repositories::test_support::create_test_monitor;

    fn rec(monitor_id: i32, state: &str) -> AlertHistoryInsert {
        AlertHistoryInsert {
            monitor_id,
            alert_type: "HTTP".to_string(),
            state: state.to_string(),
            message: Some(format!("msg-{state}")),
        }
    }

    #[test]
    fn insert_and_keyset_paginate() {
        let dir = tempfile::tempdir().unwrap();
        let pool = Arc::new(test_pool(dir.path()));
        let mid = create_test_monitor(&pool, "h");
        let repo = AlertHistoryRepository::new(pool);
        for s in ["triggered", "recovered", "triggered", "recovered"] {
            repo.insert(&rec(mid, s)).unwrap();
        }
        // 第一页取 2 条（最新在前，id 降序）
        let page1 = repo.get_history(Some(mid), None, 2).unwrap();
        assert_eq!(page1.len(), 2);
        assert!(page1[0].id > page1[1].id, "应按 id 降序");
        // 下一页：before_id = 第一页末条 id
        let cursor = page1.last().unwrap().id;
        let page2 = repo.get_history(Some(mid), Some(cursor), 2).unwrap();
        assert_eq!(page2.len(), 2);
        assert!(page2.iter().all(|r| r.id < cursor), "游标之前的记录");
        // 第三页应为空（共 4 条）
        let cursor2 = page2.last().unwrap().id;
        assert!(
            repo.get_history(Some(mid), Some(cursor2), 2)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn filter_by_monitor_id() {
        let dir = tempfile::tempdir().unwrap();
        let pool = Arc::new(test_pool(dir.path()));
        let a = create_test_monitor(&pool, "a");
        let b = create_test_monitor(&pool, "b");
        let repo = AlertHistoryRepository::new(pool);
        repo.insert(&rec(a, "triggered")).unwrap();
        repo.insert(&rec(b, "triggered")).unwrap();
        repo.insert(&rec(b, "recovered")).unwrap();
        assert_eq!(repo.get_history(Some(a), None, 50).unwrap().len(), 1);
        assert_eq!(repo.get_history(Some(b), None, 50).unwrap().len(), 2);
        assert_eq!(repo.get_history(None, None, 50).unwrap().len(), 3);
    }
}
