// /metrics 端点处理器：输出Prometheus文本格式指标，供Prometheus/Grafana等抓取
use std::sync::Arc;

use actix_web::web;

use crate::database::pool::SqlitePool;
use crate::database::repositories::alert_state_repo::AlertStateRepository;
use crate::metrics::MetricsRegistry;

// GET /metrics：内存指标注册表渲染 + alert_state表快照合并
// 查库失败时省略alerting家族而不是报500：指标端点应尽力输出
pub async fn get_metrics(
    registry: web::Data<Arc<MetricsRegistry>>,
    pool: web::Data<Arc<SqlitePool>>,
) -> impl actix_web::Responder {
    // 同步 diesel 查询挪阻塞池（ADR-0002 A1）；查库/阻塞任何一层失败都降级为
    // None（省略 alerting 家族），指标端点应尽力输出而不是报 500
    let pool = pool.get_ref().clone();
    let alerting = web::block(move || AlertStateRepository::new(pool).get_all_alerting())
        .await
        .ok()
        .and_then(|r| r.ok());
    // MetricsRegistry 内部自带带中毒恢复的 Mutex（ADR-0007 D1：去掉冗余的外层 Arc<Mutex>）
    let body = registry.render(alerting.as_deref());
    actix_web::HttpResponse::Ok()
        .content_type("text/plain; version=0.0.4; charset=utf-8")
        .body(body)
}
