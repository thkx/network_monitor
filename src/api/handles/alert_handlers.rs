// /api/alerts 端点：按 keyset（游标）分页读取告警历史（触发/恢复事件）
// 与 /api/results 同模式：before_id 为游标，返回 next_cursor；不返回 total
use actix_web::{HttpResponse, web};
use serde::Deserialize;
use std::sync::Arc;

use super::response::{DefaultResponseObj, clamp_pagination, internal_error};
use super::result_handlers::CursorPageData;
use crate::database::pool::SqlitePool;
use crate::database::repositories::alert_history_repo::AlertHistoryRepository;

#[derive(Debug, Deserialize)]
pub struct AlertQueryParams {
    pub monitor_id: Option<i32>, // 可选：按监控项过滤
    pub before_id: Option<i32>,  // 游标：取该 id 之前的一页；缺省取最新一页
    pub page_size: Option<i64>,
}

// GET /api/alerts?monitor_id=&before_id=&page_size=
// keyset 分页：按 id 降序返回一页，满页时以末条 id 作为 next_cursor
pub async fn get_alerts(
    pool: web::Data<Arc<SqlitePool>>,
    query: web::Query<AlertQueryParams>,
) -> Result<HttpResponse, actix_web::Error> {
    // page_size 复用统一夹取逻辑（1..=500）；page_no 在游标模式下无意义
    let (_, size) = clamp_pagination(None, query.page_size);
    let repo = AlertHistoryRepository::new(pool.get_ref().clone());
    let list = repo
        .get_history(query.monitor_id, query.before_id, size)
        .map_err(internal_error)?;
    // 满页时以最后一条 id 作为下一页游标；不足一页说明已到末页
    let next_cursor = if (list.len() as i64) == size {
        list.last().map(|r| r.id)
    } else {
        None
    };
    Ok(HttpResponse::Ok().json(DefaultResponseObj {
        code: 200,
        message: "OK".to_string(),
        data: CursorPageData {
            list,
            next_cursor,
            page_size: size,
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::get_alerts;
    use crate::database::models::AlertHistoryInsert;
    use crate::database::pool::test_pool;
    use crate::database::repositories::alert_history_repo::AlertHistoryRepository;
    use crate::database::repositories::test_support::create_test_monitor;
    use actix_web::{App, test, web};
    use serde::Deserialize;
    use std::sync::Arc;

    #[derive(Debug, Deserialize)]
    struct Resp {
        code: i32,
        data: Data,
    }
    #[derive(Debug, Deserialize)]
    struct Data {
        list: Vec<Row>,
        next_cursor: Option<i32>,
        page_size: i64,
    }
    #[derive(Debug, Deserialize)]
    struct Row {
        id: i32,
        state: String,
        alert_type: String,
    }

    #[actix_web::test]
    async fn get_alerts_keyset_paginates() {
        let dir = tempfile::tempdir().unwrap();
        let pool = Arc::new(test_pool(dir.path()));
        let mid = create_test_monitor(&pool, "h");
        let repo = AlertHistoryRepository::new(pool.clone());
        for s in ["triggered", "recovered", "triggered"] {
            repo.insert(&AlertHistoryInsert {
                monitor_id: mid,
                alert_type: "HTTP".to_string(),
                state: s.to_string(),
                message: Some(format!("m-{s}")),
            })
            .unwrap();
        }
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(pool.clone()))
                .route("/api/alerts", web::get().to(get_alerts)),
        )
        .await;

        // 第一页取 2 条（最新在前）
        let req = test::TestRequest::get()
            .uri("/api/alerts?page_size=2")
            .to_request();
        let resp: Resp = test::call_and_read_body_json(&app, req).await;
        assert_eq!(resp.code, 200);
        assert_eq!(resp.data.page_size, 2);
        assert_eq!(resp.data.list.len(), 2);
        assert_eq!(resp.data.list[0].alert_type, "HTTP");
        assert!(resp.data.list[0].id > resp.data.list[1].id);
        let cursor = resp.data.next_cursor.expect("满页应有游标");

        // 下一页：before_id=游标，剩 1 条，next_cursor 为 None
        let req = test::TestRequest::get()
            .uri(&format!("/api/alerts?page_size=2&before_id={cursor}"))
            .to_request();
        let resp: Resp = test::call_and_read_body_json(&app, req).await;
        assert_eq!(resp.data.list.len(), 1);
        assert!(resp.data.next_cursor.is_none());
        assert_eq!(resp.data.list[0].state, "triggered");
    }
}
