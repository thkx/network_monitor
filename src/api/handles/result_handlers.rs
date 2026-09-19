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
