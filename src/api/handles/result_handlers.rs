use actix_web::{web, HttpResponse};
use serde::Deserialize;

use super::monitor_handlers::{DefaultResponseObj, PageData};
use crate::database::services::result_service::ResultService;

#[derive(Debug, Deserialize)]
pub struct ResultQueryParams {
    pub monitor_id: Option<i32>, // 可选：按监控项过滤
    pub page_no: Option<i64>,
    pub page_size: Option<i64>,
}

// 查询监控结果（可按monitor_id过滤，支持分页）
pub async fn get_check_results(
    result_service: web::Data<ResultService>,
    query: web::Query<ResultQueryParams>,
) -> Result<HttpResponse, actix_web::Error> {
    let (no, size) = super::monitor_handlers::clamp_pagination(query.page_no, query.page_size);
    let (list, total) = result_service
        .get_check_results(query.monitor_id, no, size)
        .map_err(|e| {
            actix_web::error::ErrorInternalServerError(format!(
                "Failed to get check results: {}",
                e
            ))
        })?;
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
