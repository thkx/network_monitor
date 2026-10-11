// /healthz 存活探针：无认证、无依赖，仅表明进程在正常提供 HTTP 服务。
// 定位为 liveness（存活）而非 readiness（就绪）——不查库，避免 DB 抖动时被误杀重启。
// /readyz 就绪探针（ADR-0007 D10）：轻量校验依赖（DB 连接池可取连接且可执行查询）可用，
// 供编排器/负载均衡据此摘流或延迟引流——与 liveness 分离语义，避免 DB 池耗尽时 /healthz
// 仍返回绿、请求却已失败。
use actix_web::{HttpResponse, web};
use diesel::connection::SimpleConnection;
use serde_json::json;
use std::sync::Arc;

use crate::database::pool::{SqlitePool, get_connection};

// GET /healthz：恒定返回 200，供容器/编排器/负载均衡免认证探活
pub async fn get_healthz() -> HttpResponse {
    HttpResponse::Ok().json(json!({ "status": "ok" }))
}

// GET /readyz：轻量校验数据库就绪（取连接 + SELECT 1）。
// 就绪→200 {"status":"ready"}；依赖不可用→503 {"status":"not_ready"}（不含错误细节）。
// 同步 diesel 校验挪到阻塞线程池执行（ADR-0002 A1），避免占用 runtime 线程。
pub async fn get_readyz(pool: web::Data<Arc<SqlitePool>>) -> HttpResponse {
    let pool = pool.get_ref().clone();
    let check = web::block(move || {
        let mut conn = get_connection(&pool)?;
        conn.batch_execute("SELECT 1")?;
        Ok::<(), diesel::result::Error>(())
    })
    .await;
    match check {
        Ok(Ok(())) => HttpResponse::Ok().json(json!({ "status": "ready" })),
        Ok(Err(e)) => {
            // 依赖错误仅落日志、不回客户端（避免泄漏表名/SQL 片段，ADR-0006 E1）
            tracing::warn!("就绪探针校验数据库失败: {}", e);
            HttpResponse::ServiceUnavailable().json(json!({ "status": "not_ready" }))
        }
        Err(e) => {
            tracing::warn!("就绪探针阻塞任务失败: {}", e);
            HttpResponse::ServiceUnavailable().json(json!({ "status": "not_ready" }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{get_healthz, get_readyz};
    use crate::database::pool::test_pool;
    use actix_web::{App, test, web};
    use std::sync::Arc;

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

    // 就绪探针：DB 可用时返回 200 ready（校验走真实临时库 + SELECT 1）
    #[actix_web::test]
    async fn readyz_returns_200_when_db_ready() {
        let dir = tempfile::tempdir().expect("临时目录创建失败");
        let pool = Arc::new(test_pool(dir.path()));
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(pool))
                .route("/readyz", web::get().to(get_readyz)),
        )
        .await;
        let resp =
            test::call_service(&app, test::TestRequest::get().uri("/readyz").to_request()).await;
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["status"], "ready");
    }
}
