// API 通用响应类型与分页工具：统一返回信封、分页数据结构、分页参数防御、错误转换
// （从 monitor_handlers 抽出，供各 handler 模块共享，避免跨 handler 的横向依赖）
use actix_web::web;
use serde::{Deserialize, Serialize};

// 定义统一的返回数据结构
#[derive(Debug, Serialize, Deserialize)]
pub struct DefaultResponseObj<T> {
    pub code: usize,
    pub message: String,
    pub data: T,
}

#[derive(Debug, Deserialize)]
pub struct PaginationParams {
    pub page_no: Option<i64>,
    pub page_size: Option<i64>,
    pub enabled: Option<bool>, // 可选：true/false筛选启停状态，缺省查询全部
    pub tag: Option<String>,   // 可选：按分组标签精确筛选，缺省不限
}

// 分页返回数据
#[derive(Debug, Serialize)]
pub struct PageData<T: Serialize> {
    pub list: Vec<T>,
    pub total: i64,
    pub page_no: i64,
    pub page_size: i64,
}

// 分页参数防御：page_no≥1，page_size限制在1..=500
// （SQLite里LIMIT为负等价于无限制，page_size=-1即全表导出；超大值会整表拉进内存）
pub fn clamp_pagination(page_no: Option<i64>, page_size: Option<i64>) -> (i64, i64) {
    let no = page_no.unwrap_or(1).max(1);
    let size = page_size.unwrap_or(20).clamp(1, 500);
    (no, size)
}

// 统一500错误转换：详细错误仅落日志（可能含表名/SQL片段等内部信息），
// 客户端只回通用文案，避免数据库错误原文泄露到响应体（ADR-0006 E1 脱敏）
pub fn internal_error(e: diesel::result::Error) -> actix_web::Error {
    tracing::error!("数据库错误: {}", e);
    actix_web::error::ErrorInternalServerError("内部错误")
}

// 在 actix 阻塞线程池执行同步 diesel 操作，收敛 `web::block` + 双层错误映射样板
// （ADR-0002 A1）：外层 BlockingError（线程池调度/任务 panic）与内层 diesel::Error
// 都脱敏为通用 500，详细原因落 tracing::error（ADR-0006 E1）。
// 闭包返回 Result<T, diesel::result::Error>，与各 service/repo 方法签名一致。
pub async fn run_blocking<F, T>(f: F) -> Result<T, actix_web::Error>
where
    F: FnOnce() -> Result<T, diesel::result::Error> + Send + 'static,
    T: Send + 'static,
{
    web::block(f)
        .await
        .map_err(|e| {
            tracing::error!("阻塞任务执行失败: {}", e);
            actix_web::error::ErrorInternalServerError("内部错误")
        })?
        .map_err(internal_error)
}

#[cfg(test)]
mod tests {
    use super::clamp_pagination;

    #[test]
    fn pagination_is_clamped() {
        // 缺省值
        assert_eq!(clamp_pagination(None, None), (1, 20));
        // 负数size在SQLite里等价LIMIT无限制（全表导出），必须夹到1
        assert_eq!(clamp_pagination(Some(0), Some(-1)), (1, 1));
        // 超大size夹到500上限
        assert_eq!(clamp_pagination(Some(3), Some(100000)), (3, 500));
        // 正常值原样通过
        assert_eq!(clamp_pagination(Some(2), Some(50)), (2, 50));
    }
}
