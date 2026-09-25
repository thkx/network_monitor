// 控制台聚合视图：一次请求拿齐列表页所需全部数据（配置 + 各自最新一次检查结果 + 告警抑制状态）
// 前端无需逐个监控再查结果。（从 monitor_handlers 抽出：聚合读取逻辑独立于 CRUD 编排）
use actix_web::{HttpResponse, web};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

use super::response::{DefaultResponseObj, internal_error};
use crate::database::models::CheckResultModel;
use crate::database::pool::SqlitePool;
use crate::database::repositories::alert_state_repo::AlertStateRepository;
use crate::database::services::monitor_service::MonitorService;
use crate::database::services::result_service::ResultService;

// 控制台聚合视图单项：监控配置 + 最新一次检查结果 + 告警抑制状态
#[derive(Debug, Serialize, Deserialize)]
pub struct MonitorStatusItem {
    pub id: i32,
    pub name: String,
    pub target: String,
    pub monitor_type: String,
    pub enabled: i32,
    pub last_status: Option<i32>, // None=尚未执行过检查
    pub last_response_time: Option<i32>,
    pub last_check_at: Option<String>,
    pub alerting: bool, // 是否处于"已告警未恢复"状态
    pub tag: Option<String>, // 分组标签（用于列表页筛选）
}

// GET /api/status
pub async fn get_console_status(
    monitor_service: web::Data<MonitorService>,
    result_service: web::Data<ResultService>,
    pool: web::Data<Arc<SqlitePool>>,
) -> Result<HttpResponse, actix_web::Error> {
    // 全量DB读取挪阻塞池：此端点是登录控制台每5秒轮询的最高频DB调用，
    // 同步diesel在worker线程执行会卡runtime（与flush/清理/reload同款规则）。
    // 全量读取而非分页接口硬编码1000上限：配置超过1000时状态页不再静默截尾
    let (monitors, latest, alerting) = web::block({
        let monitor_service = monitor_service.get_ref().clone();
        let result_service = result_service.get_ref().clone();
        let pool = pool.get_ref().clone();
        move || -> Result<_, diesel::result::Error> {
            let monitors = monitor_service.get_all_monitors()?;
            let monitor_ids: Vec<i32> = monitors.iter().map(|m| m.id).collect();
            // latest_per_monitor逐监控索引化查询（走复合索引），不再全表扫描
            let latest = result_service.get_latest_by_monitor(&monitor_ids)?;
            // 查库失败降级为空快照：状态页仍可展示，只是alerting全为false
            let alerting = AlertStateRepository::new(pool)
                .get_all_alerting()
                .unwrap_or_default();
            Ok((monitors, latest, alerting))
        }
    })
    .await
    .map_err(|e| actix_web::error::ErrorInternalServerError(format!("状态读取阻塞任务失败: {e}")))?
    .map_err(internal_error)?;
    let latest_map: HashMap<i32, &CheckResultModel> =
        latest.iter().map(|r| (r.monitor_id, r)).collect();
    let alert_map: HashMap<i32, bool> = alerting.into_iter().collect();
    let items: Vec<MonitorStatusItem> = monitors
        .iter()
        .map(|m| {
            let l = latest_map.get(&m.id);
            MonitorStatusItem {
                id: m.id,
                name: m.name.clone().unwrap_or_else(|| m.target.clone()),
                target: m.target.clone(),
                monitor_type: m.monitor_type.clone(),
                enabled: m.enabled,
                last_status: l.map(|r| r.status),
                last_response_time: l.map(|r| r.response_time),
                last_check_at: l.and_then(|r| {
                    r.created_at
                        .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
                }),
                alerting: alert_map.get(&m.id).copied().unwrap_or(false),
                tag: m.tag.clone(),
            }
        })
        .collect();
    Ok(HttpResponse::Ok().json(DefaultResponseObj {
        code: 200,
        message: "OK".to_string(),
        data: items,
    }))
}

#[cfg(test)]
mod tests {
    use super::{MonitorStatusItem, get_console_status};
    use crate::api::handles::monitor_handlers::run_monitor_once;
    use crate::api::handles::response::DefaultResponseObj;
    use crate::database::models::MonitorConfigInsert;
    use crate::database::pool::test_pool;
    use crate::database::repositories::monitor_repo::MonitorRepository;
    use crate::database::repositories::result_repo::CheckResultRepository;
    use crate::database::services::monitor_service::MonitorService;
    use crate::database::services::result_service::ResultService;
    use crate::metrics::MetricsRegistry;
    use actix_web::web;
    use std::sync::{Arc, Mutex};

    // 控制台两个端点的端到端测试：临时库 + 内存App
    // 手动执行选CPU类型监控：无网络依赖，结果稳定可用
    #[actix_web::test]
    async fn console_status_and_manual_run_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let pool = Arc::new(test_pool(dir.path()));
        let monitor_service = MonitorService::new(MonitorRepository::new(pool.clone()));
        let result_service = ResultService::new(CheckResultRepository::new(pool.clone()));
        let metrics = Arc::new(Mutex::new(MetricsRegistry::new()));

        let insert = MonitorConfigInsert {
            name: Some("t-cpu".to_string()),
            target: "cpu://local".to_string(),
            method: None,
            monitor_type: "CPU".to_string(),
            interval_ms: Some(5),
            timeout_ms: 5000,
            config_json: Some(r#"{"monitor_type":"CPU","timeout":5}"#.to_string()),
            enabled: 1,
            tag: None,
        };
        let created = monitor_service.create_monitor(&insert).unwrap();

        let app = actix_web::test::init_service(
            actix_web::App::new()
                .app_data(web::Data::new(monitor_service.clone()))
                .app_data(web::Data::new(result_service.clone()))
                .app_data(web::Data::new(pool.clone()))
                .app_data(web::Data::new(metrics.clone()))
                .app_data(web::Data::new(5u64))
                .route("/api/status", web::get().to(get_console_status))
                .route("/api/monitors/{id}/run", web::post().to(run_monitor_once)),
        )
        .await;

        // 手动执行：200 + 可用 + 详情返回
        let req = actix_web::test::TestRequest::post()
            .uri(&format!("/api/monitors/{}/run", created.id))
            .to_request();
        let resp = actix_web::test::call_service(&app, req).await;
        assert_eq!(resp.status(), 200, "手动执行应成功");
        let body: DefaultResponseObj<serde_json::Value> =
            actix_web::test::read_body_json(resp).await;
        assert_eq!(body.data["status"], true);
        assert!(body.data["details"].is_object());

        // status聚合：最新结果已落库可见
        let req = actix_web::test::TestRequest::get()
            .uri("/api/status")
            .to_request();
        let resp = actix_web::test::call_service(&app, req).await;
        assert_eq!(resp.status(), 200);
        let body: DefaultResponseObj<Vec<MonitorStatusItem>> =
            actix_web::test::read_body_json(resp).await;
        assert_eq!(body.data.len(), 1);
        assert_eq!(body.data[0].id, created.id);
        assert_eq!(body.data[0].last_status, Some(1), "最新结果应为可用");
        assert!(body.data[0].last_check_at.is_some());
        assert!(!body.data[0].alerting);

        // 手动执行不存在的监控：404
        let req = actix_web::test::TestRequest::post()
            .uri("/api/monitors/99999/run")
            .to_request();
        let resp = actix_web::test::call_service(&app, req).await;
        assert_eq!(resp.status(), 404);
    }

    // 状态聚合视图不受分页上限影响：此前硬编码 get_monitors_paged(1,1000)，
    // 配置超过1000会静默截尾；改用get_all后应全量返回
    #[actix_web::test]
    async fn console_status_returns_all_monitors_beyond_page_size() {
        let dir = tempfile::tempdir().unwrap();
        let pool = Arc::new(test_pool(dir.path()));
        let monitor_service = MonitorService::new(MonitorRepository::new(pool.clone()));
        let result_service = ResultService::new(CheckResultRepository::new(pool.clone()));
        // 25条 > 常规分页20：若仍走分页接口即会截尾
        for i in 0..25 {
            monitor_service
                .create_monitor(&MonitorConfigInsert {
                    name: Some(format!("t-status-{i}")),
                    target: format!("cpu://{i}"),
                    method: None,
                    monitor_type: "CPU".to_string(),
                    interval_ms: Some(5),
                    timeout_ms: 5000,
                    config_json: Some(r#"{"monitor_type":"CPU","timeout":5}"#.to_string()),
                    enabled: 1,
                    tag: None,
                })
                .unwrap();
        }

        let app = actix_web::test::init_service(
            actix_web::App::new()
                .app_data(web::Data::new(monitor_service.clone()))
                .app_data(web::Data::new(result_service.clone()))
                .app_data(web::Data::new(pool.clone()))
                .app_data(web::Data::new(Arc::new(std::sync::Mutex::new(
                    MetricsRegistry::new(),
                ))))
                .route("/api/status", web::get().to(get_console_status)),
        )
        .await;

        let req = actix_web::test::TestRequest::get()
            .uri("/api/status")
            .to_request();
        let resp = actix_web::test::call_service(&app, req).await;
        assert_eq!(resp.status(), 200);
        let body: DefaultResponseObj<Vec<MonitorStatusItem>> =
            actix_web::test::read_body_json(resp).await;
        assert_eq!(body.data.len(), 25, "状态页应返回全部25条配置");
    }
}
