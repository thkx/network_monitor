// /healthz 存活探针：无认证、无依赖，仅表明进程在正常提供 HTTP 服务。
// 定位为 liveness（存活）而非 readiness（就绪）——不查库，避免 DB 抖动时被误杀重启；
// 数据库/依赖的就绪度由 /metrics 与 /api/status 观测。
use actix_web::HttpResponse;
use serde_json::json;

// GET /healthz：恒定返回 200，供容器/编排器/负载均衡免认证探活
pub async fn get_healthz() -> HttpResponse {
    HttpResponse::Ok().json(json!({ "status": "ok" }))
}

#[cfg(test)]
mod tests {
    use super::get_healthz;
    use actix_web::{App, test, web};

    #[actix_web::test]
    async fn healthz_returns_200_ok() {
        let app =
            test::init_service(App::new().route("/healthz", web::get().to(get_healthz))).await;
        let resp =
            test::call_service(&app, test::TestRequest::get().uri("/healthz").to_request()).await;
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["status"], "ok");
    }
}
