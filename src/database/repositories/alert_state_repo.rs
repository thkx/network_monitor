use crate::database::connect_db::{get_connection, SqlitePool};
use crate::database::models::{AlertStateModel, AlertStateModelInsert};
use crate::database::schema::alert_state;
use diesel::prelude::*;
use std::sync::Arc;

// 告警抑制状态仓库：持久化每个监控的"已告警未恢复"标志
// 告警引擎（AlertsEngine）作为领域组件直接使用本仓库，无需再包一层Service
pub struct AlertStateRepository {
    pool: Arc<SqlitePool>,
}

impl AlertStateRepository {
    pub fn new(pool: Arc<SqlitePool>) -> Self {
        AlertStateRepository { pool }
    }

    // 读取抑制状态；无记录视为未告警
    pub fn get_alerting(&self, monitor_id: i32) -> Result<bool, diesel::result::Error> {
        let mut conn = get_connection(&self.pool);
        let state: Option<AlertStateModel> = alert_state::table
            .find(monitor_id)
            .first::<AlertStateModel>(&mut conn)
            .optional()?;
        Ok(state.map(|s| s.alerting == 1).unwrap_or(false))
    }

    // 保存抑制状态（upsert：不存在则插入，存在则更新）
    pub fn set_alerting(&self, monitor_id: i32, alerting: bool) -> Result<(), diesel::result::Error> {
        let mut conn = get_connection(&self.pool);
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
