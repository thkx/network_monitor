use actix_web::{web, HttpResponse};
use serde::Deserialize;

use super::response::{clamp_pagination, DefaultResponseObj, PageData};
use crate::database::services::result_service::ResultService;

use serde::Serialize;

#[derive(Debug, Deserialize)]
pub struct ResultQueryParams {
    pub monitor_id: Option<i32>, // 可选：按监控项过滤
    pub page_no: Option<i64>,
    pub page_size: Option<i64>,
    // 游标分页：提供 before_id 时走 keyset 查询（取该 id 之前的一页），
    // 翻页代价与页码无关，适合大表深翻页。缺省（不传 before_id 且 cursor=false）
    // 走原有 OFFSET 分页，保持既有调用方（控制台 page_no/page_size）兼容
    pub before_id: Option<i32>,
    #[serde(default)]
    pub cursor: bool, // 显式请求游标模式（首页无 before_id 时用）
}

// 游标分页返回数据：next_cursor 为 None 表示已到末页
#[derive(Debug, Serialize)]
pub struct CursorPageData<T: Serialize> {
    pub list: Vec<T>,
    pub next_cursor: Option<i32>,
    pub page_size: i64,
}

// 查询监控结果（可按monitor_id过滤）：
//   - 传 before_id 或 cursor=true → 游标(keyset)分页，返回 next_cursor
//   - 否则 → OFFSET 分页（page_no/page_size），返回 total（兼容既有控制台）
pub async fn get_check_results(
    result_service: web::Data<ResultService>,
    query: web::Query<ResultQueryParams>,
) -> Result<HttpResponse, actix_web::Error> {
    let internal = |e: diesel::result::Error| {
        actix_web::error::ErrorInternalServerError(format!("Failed to get check results: {}", e))
    };
    // 游标模式：显式 cursor=true 或已带 before_id
    if query.cursor || query.before_id.is_some() {
        // page_size 复用同一夹取逻辑（1..=500）；page_no 在游标模式下忽略
        let (_, size) = clamp_pagination(None, query.page_size);
        let list = result_service
            .get_check_results_keyset(query.monitor_id, query.before_id, size)
            .map_err(internal)?;
        // 满页时以最后一条 id 作为下一页游标；不足一页说明已到末页
        let next_cursor = if (list.len() as i64) == size {
            list.last().map(|r| r.id)
        } else {
            None
        };
        return Ok(HttpResponse::Ok().json(DefaultResponseObj {
            code: 200,
            message: "OK".to_string(),
            data: CursorPageData {
                list,
                next_cursor,
                page_size: size,
            },
        }));
    }
    let (no, size) = clamp_pagination(query.page_no, query.page_size);
    let (list, total) = result_service
        .get_check_results(query.monitor_id, no, size)
        .map_err(internal)?;
    Ok(HttpResponse::Ok().json(DefaultResponseObj {
        code: 200,
        message: "OK".to_string(),
        data: PageData {
            list,
            total,
            page_no: no,
            page_size: size,
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::get_check_results;
    use crate::database::pool::test_pool;
    use crate::database::models::{CheckResultModel, CheckResultModelInsert};
    use crate::database::repositories::result_repo::CheckResultRepository;
    use crate::database::repositories::test_support::create_test_monitor;
    use crate::database::services::result_service::ResultService;
    use actix_web::web;
    use serde::Deserialize;
    use std::sync::Arc;

    // 端到端断言用：与 handler 返回的 DefaultResponseObj<CursorPageData/PageData> 对齐
    #[derive(Debug, Deserialize)]
    struct CursorResp {
        code: i32,
        data: CursorData,
    }
    #[derive(Debug, Deserialize)]
    struct CursorData {
        list: Vec<CheckResultModel>,
        next_cursor: Option<i32>,
        page_size: i64,
    }
    #[derive(Debug, Deserialize)]
    struct OffsetResp {
        data: OffsetData,
    }
    #[derive(Debug, Deserialize)]
    #[allow(dead_code)] // list 未直接断言但需反序列化对齐字段
    struct OffsetData {
        list: Vec<CheckResultModel>,
        total: i64,
        page_no: i64,
        page_size: i64,
    }

    // 建库、插 n 条结果、返回 (tempdir守卫, ResultService, monitor_id)。
    // tempdir 守卫须由调用方持有到测试结束，否则库文件被提前删除
    fn seed(tag: &str, n: i32) -> (tempfile::TempDir, ResultService, i32) {
        let dir = tempfile::tempdir().unwrap();
        let pool = Arc::new(test_pool(dir.path()));
        let mid = create_test_monitor(&pool, tag);
        let repo = CheckResultRepository::new(pool.clone());
        for i in 1..=n {
            repo.insert_check_result(&CheckResultModelInsert {
                monitor_id: mid,
                monitor_type: "HTTP".to_string(),
                status: 1,
                response_time: i * 10,
                metadata_json: None,
            })
            .unwrap();
        }
        (dir, ResultService::new(CheckResultRepository::new(pool)), mid)
    }

    // 游标模式（cursor=true）逐页走完：跨页无重叠、满页给 next_cursor、末页给 None
    #[actix_web::test]
    async fn cursor_pagination_endpoint_walks_all_pages() {
        let (_dir, result_service, mid) = seed("t-rh-cursor", 5);
        let app = actix_web::test::init_service(
            actix_web::App::new()
                .app_data(web::Data::new(result_service))
                .route("/api/results", web::get().to(get_check_results)),
        )
        .await;

        // 首页：cursor=true，page_size=2 → 取最新2条，next_cursor=末条id
        let req = actix_web::test::TestRequest::get()
            .uri(&format!("/api/results?monitor_id={mid}&cursor=true&page_size=2"))
            .to_request();
        let resp = actix_web::test::call_service(&app, req).await;
        assert_eq!(resp.status(), 200);
        let body: CursorResp = actix_web::test::read_body_json(resp).await;
        assert_eq!(body.code, 200);
        assert_eq!(body.data.page_size, 2);
        assert_eq!(body.data.list.len(), 2);
        assert!(body.data.list[0].id > body.data.list[1].id, "id降序");
        assert_eq!(body.data.list[0].response_time, 50, "最新一条在前");
        let c1 = body.data.next_cursor.expect("满页应给next_cursor");
        assert_eq!(c1, body.data.list[1].id);

        // 第二页：before_id=c1 → 再取2条，均在游标之前
        let req = actix_web::test::TestRequest::get()
            .uri(&format!("/api/results?monitor_id={mid}&before_id={c1}&page_size=2"))
            .to_request();
        let body: CursorResp =
            actix_web::test::read_body_json(actix_web::test::call_service(&app, req).await).await;
        assert_eq!(body.data.list.len(), 2);
        assert!(body.data.list.iter().all(|r| r.id < c1), "严格在游标之前");
        let c2 = body.data.next_cursor.expect("仍满页");

        // 末页：剩1条，不足page_size → next_cursor=None
        let req = actix_web::test::TestRequest::get()
            .uri(&format!("/api/results?monitor_id={mid}&before_id={c2}&page_size=2"))
            .to_request();
        let body: CursorResp =
            actix_web::test::read_body_json(actix_web::test::call_service(&app, req).await).await;
        assert_eq!(body.data.list.len(), 1);
        assert_eq!(body.data.list[0].response_time, 10, "最旧一条");
        assert!(body.data.next_cursor.is_none(), "不足一页应无next_cursor");
    }

    // 不传 cursor/before_id → 走 OFFSET 分页，返回 total/page_no（兼容既有控制台契约）
    #[actix_web::test]
    async fn offset_pagination_is_default_and_returns_total() {
        let (_dir, result_service, mid) = seed("t-rh-offset", 3);
        let app = actix_web::test::init_service(
            actix_web::App::new()
                .app_data(web::Data::new(result_service))
                .route("/api/results", web::get().to(get_check_results)),
        )
        .await;
        let req = actix_web::test::TestRequest::get()
            .uri(&format!("/api/results?monitor_id={mid}&page_no=1&page_size=10"))
            .to_request();
        let resp = actix_web::test::call_service(&app, req).await;
        assert_eq!(resp.status(), 200);
        let body: OffsetResp = actix_web::test::read_body_json(resp).await;
        assert_eq!(body.data.total, 3, "OFFSET模式应带total");
        assert_eq!(body.data.page_no, 1);
        assert_eq!(body.data.page_size, 10);
        assert_eq!(body.data.list.len(), 3);
        assert_eq!(body.data.list[0].response_time, 30, "id降序，最新在前");
    }
}
