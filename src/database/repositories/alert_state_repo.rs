use crate::database::pool::{get_connection, SqlitePool};
use crate::database::models::{AlertStateModel, AlertStateModelInsert};
use crate::database::schema::alert_state;
use diesel::prelude::*;
use std::sync::Arc;

// 告警抑制状态仓库：持久化每个监控的"已告警未恢复"标志
// 告警引擎（AlertsEngine）作为领域组件直接使用本仓库，无需再包一层Service
// Clone仅复制Arc句柄，供persist_state挪入spawn_blocking使用
#[derive(Clone)]
pub struct AlertStateRepository {
    pool: Arc<SqlitePool>,
}

impl AlertStateRepository {
    pub fn new(pool: Arc<SqlitePool>) -> Self {
        AlertStateRepository { pool }
    }

    // 读取抑制状态；无记录视为未告警
    pub fn get_alerting(&self, monitor_id: i32) -> Result<bool, diesel::result::Error> {
        let mut conn = get_connection(&self.pool)?;
        let state: Option<AlertStateModel> = alert_state::table
            .find(monitor_id)
            .first::<AlertStateModel>(&mut conn)
            .optional()?;
        Ok(state.map(|s| s.alerting == 1).unwrap_or(false))
    }

    // 读取全部抑制状态（/metrics 端点渲染 monitor_alerting 用）
    pub fn get_all_alerting(&self) -> Result<Vec<(i32, bool)>, diesel::result::Error> {
        let mut conn = get_connection(&self.pool)?;
        let rows: Vec<(i32, i32)> = alert_state::table
            .select((alert_state::monitor_id, alert_state::alerting))
            .load(&mut conn)?;
        Ok(rows.into_iter().map(|(id, a)| (id, a == 1)).collect())
    }

    // 保存抑制状态（upsert：不存在则插入，存在则更新）
    pub fn set_alerting(&self, monitor_id: i32, alerting: bool) -> Result<(), diesel::result::Error> {
        let mut conn = get_connection(&self.pool)?;
        let alerting_v = if alerting { 1 } else { 0 };
        diesel::insert_into(alert_state::table)
            .values(AlertStateModelInsert {
                monitor_id,
                alerting: alerting_v,
            })
            .on_conflict(alert_state::monitor_id)
            .do_update()
            .set(alert_state::alerting.eq(alerting_v))
            .execute(&mut conn)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::AlertStateRepository;
    use crate::database::pool::test_pool;
    use crate::database::repositories::test_support::create_test_monitor;
    use std::sync::Arc;

    #[test]
    fn alert_state_upsert_roundtrip() {
        let dir = tempfile::tempdir().expect("临时目录创建失败");
        let pool = Arc::new(test_pool(dir.path()));
        let id_a = create_test_monitor(&pool, "t-a");
        let id_b = create_test_monitor(&pool, "t-b");
        let repo = AlertStateRepository::new(pool);
        // 无记录时默认未告警
        assert!(!repo.get_alerting(id_a).unwrap());
        // 置为告警中
        repo.set_alerting(id_a, true).unwrap();
        assert!(repo.get_alerting(id_a).unwrap());
        // 重复写入走upsert更新而非插入
        repo.set_alerting(id_a, true).unwrap();
        assert!(repo.get_alerting(id_a).unwrap());
        // 恢复后写回false
        repo.set_alerting(id_a, false).unwrap();
        assert!(!repo.get_alerting(id_a).unwrap());
        // 不同monitor之间互不影响
        repo.set_alerting(id_b, true).unwrap();
        assert!(!repo.get_alerting(id_a).unwrap());
        assert!(repo.get_alerting(id_b).unwrap());
    }
}
