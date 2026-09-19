// API 通用响应类型与分页工具：统一返回信封、分页数据结构、分页参数防御、错误转换
// （从 monitor_handlers 抽出，供各 handler 模块共享，避免跨 handler 的横向依赖）
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

// 统一500错误转换
pub fn internal_error(e: diesel::result::Error) -> actix_web::Error {
    actix_web::error::ErrorInternalServerError(format!("Database error: {}", e))
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
