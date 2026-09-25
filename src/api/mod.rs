pub mod handles;

use actix_web::web;
use handles::alert_handlers;
use handles::auth_handlers;
use handles::console_handlers;
use handles::health_handlers;
use handles::metrics_handlers;
use handles::monitor_handlers;
use handles::result_handlers;
use handles::status_handlers;

// 统一注册Web路由：
//   / 控制台、/login /logout 认证、/metrics 指标端点、/api/* 业务接口
// （/login与控制台静态页在Auth中间件中放行；其余受会话保护）
pub fn configure_routes(cfg: &mut web::ServiceConfig) {
    cfg.route("/", web::get().to(console_handlers::get_console));
    cfg.route("/login", web::post().to(auth_handlers::login));
    cfg.route("/logout", web::post().to(auth_handlers::logout));
    cfg.route("/metrics", web::get().to(metrics_handlers::get_metrics));
    cfg.route("/healthz", web::get().to(health_handlers::get_healthz));
    cfg.service(
        web::scope("/api")
            .route(
                "/monitors",
                web::get().to(monitor_handlers::get_all_monitors),
            )
            .route(
                "/monitors",
                web::post().to(monitor_handlers::create_monitor),
            )
            // 必须先于 /monitors/{id}，否则 "tags" 会被当作 id 捕获
            .route(
                "/monitors/tags",
                web::get().to(monitor_handlers::get_monitor_tags),
            )
            .route(
                "/monitors/{id}",
                web::get().to(monitor_handlers::get_monitor_by_id),
            )
            .route(
                "/monitors/{id}",
                web::put().to(monitor_handlers::update_monitor),
            )
            .route(
                "/monitors/{id}",
                web::delete().to(monitor_handlers::delete_monitor),
            )
            .route(
                "/monitors/{id}/enabled",
                web::patch().to(monitor_handlers::update_monitor_enabled),
            )
            .route(
                "/monitors/{id}/run",
                web::post().to(monitor_handlers::run_monitor_once),
            )
            .route(
                "/status",
                web::get().to(status_handlers::get_console_status),
            )
            .route(
                "/results",
                web::get().to(result_handlers::get_check_results),
            )
            .route("/alerts", web::get().to(alert_handlers::get_alerts)),
    );
}
