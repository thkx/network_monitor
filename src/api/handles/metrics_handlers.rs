// /metrics 端点处理器：输出Prometheus文本格式指标，供Prometheus/Grafana等抓取
use std::sync::{Arc, Mutex};

use actix_web::web;

use crate::database::pool::SqlitePool;
use crate::database::repositories::alert_state_repo::AlertStateRepository;
use crate::metrics::MetricsRegistry;

// GET /metrics：内存指标注册表渲染 + alert_state表快照合并
// 查库失败时省略alerting家族而不是报500：指标端点应尽力输出
pub async fn get_metrics(
    registry: web::Data<Arc<Mutex<MetricsRegistry>>>,
    pool: web::Data<Arc<SqlitePool>>,
) -> impl actix_web::Responder {
    let alerting = AlertStateRepository::new(pool.get_ref().clone())
        .get_all_alerting()
        .ok();
    let body = registry
        .lock()
        .expect("metrics锁中毒")
        .render(alerting.as_deref());
    actix_web::HttpResponse::Ok()
        .content_type("text/plain; version=0.0.4; charset=utf-8")
        .body(body)
}
