use chrono::NaiveDateTime;
use diesel::prelude::*;
use serde::{Deserialize, Serialize};

use crate::database::schema::{alert_history, alert_state, check_result, monitor_config};

#[derive(Queryable, Debug, Clone, Serialize, Deserialize)]
#[diesel(table_name = monitor_config)]
pub struct MonitorConfigModel {
    pub id: i32,
    pub name: Option<String>,
    pub target: String,
    pub method: Option<String>,
    pub monitor_type: String,
    pub interval_ms: Option<i32>,
    pub timeout_ms: i32,
    pub config_json: Option<String>,
    pub enabled: i32,
    pub tag: Option<String>,
    pub created_at: Option<NaiveDateTime>,
    pub updated_at: Option<NaiveDateTime>,
}

// 更新用结构（不含 id / created_at / updated_at）
#[derive(Debug, Clone, Serialize, Deserialize, AsChangeset)]
#[diesel(table_name = monitor_config)]
pub struct MonitorConfigUpdate {
    pub name: Option<String>,
    pub target: Option<String>,
    pub method: Option<String>,
    pub monitor_type: Option<String>,
    pub interval_ms: Option<i32>,
    pub timeout_ms: Option<i32>,
    pub config_json: Option<String>,
    pub enabled: Option<i32>,
    pub tag: Option<String>,
}
// 新增用结构体 （不包含ID ，updated_at） ）
#[derive(Insertable, Debug, Clone, Serialize, Deserialize)]
#[diesel(table_name = monitor_config)]
pub struct MonitorConfigInsert {
    pub name: Option<String>,
    pub target: String,
    pub method: Option<String>,
    pub monitor_type: String,
    pub interval_ms: Option<i32>,
    pub timeout_ms: i32,
    pub config_json: Option<String>,
    pub enabled: i32,
    pub tag: Option<String>,
}

#[derive(Insertable, Debug, Clone, Serialize, Deserialize, Selectable, Queryable)]
#[diesel(table_name = check_result)]
pub struct CheckResultModel {
    pub id: i32,
    pub monitor_id: i32,
    pub monitor_type: String,
    pub status: i32,
    pub response_time: i32,
    pub metadata_json: Option<String>,
    pub created_at: Option<NaiveDateTime>,
    pub updated_at: Option<NaiveDateTime>,
}

// 插入监控结果数据
#[derive(Insertable, Debug, Clone, Serialize, Deserialize)]
#[diesel(table_name = check_result)]
pub struct CheckResultModelInsert {
    pub monitor_id: i32,
    pub monitor_type: String,
    pub status: i32,
    pub response_time: i32,
    pub metadata_json: Option<String>,
}

// 告警抑制状态（alert_state表）：monitor_id为主键，一行对应一个监控配置
#[derive(Queryable, Selectable, Debug, Clone, Serialize, Deserialize)]
#[diesel(table_name = alert_state)]
pub struct AlertStateModel {
    pub monitor_id: i32,
    pub alerting: i32,
    pub updated_at: Option<NaiveDateTime>,
}

// 插入告警抑制状态（upsert时配合on_conflict使用）
#[derive(Insertable, Debug, Clone)]
#[diesel(table_name = alert_state)]
pub struct AlertStateModelInsert {
    pub monitor_id: i32,
    pub alerting: i32,
}

// 告警历史（alert_history表）：每次"触发/恢复"事件一行，供控制台回溯
#[derive(Queryable, Selectable, Debug, Clone, Serialize, Deserialize)]
#[diesel(table_name = alert_history)]
pub struct AlertHistoryModel {
    pub id: i32,
    pub monitor_id: i32,
    pub alert_type: String,
    pub state: String, // 'triggered' | 'recovered'
    pub message: Option<String>,
    pub created_at: Option<NaiveDateTime>,
}

// 插入告警历史（id/created_at 由库生成）
#[derive(Insertable, Debug, Clone)]
#[diesel(table_name = alert_history)]
pub struct AlertHistoryInsert {
    pub monitor_id: i32,
    pub alert_type: String,
    pub state: String,
    pub message: Option<String>,
}
