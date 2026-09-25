// 登录/登出处理器：凭证验证、限流、会话cookie下发与清除
use std::net::IpAddr;
use std::sync::{Arc, Mutex};

use actix_web::{HttpRequest, HttpResponse, web};
use serde::Deserialize;

use crate::auth::{
    AuthConfig, LoginOutcome, SESSION_COOKIE, SessionStore, build_expired_cookie,
    build_session_cookie,
};

#[derive(Debug, Deserialize)]
pub struct LoginBody {
    pub username: String,
    pub password: String,
}

// POST /login：原子登录 -> 下发HttpOnly会话cookie
pub async fn login(
    store: web::Data<Arc<Mutex<SessionStore>>>,
    config: web::Data<Arc<AuthConfig>>,
    req: HttpRequest,
    body: web::Json<LoginBody>,
) -> HttpResponse {
    if !config.enabled {
        return HttpResponse::BadRequest().json(serde_json::json!({
            "code": 400,
            "message": "认证未启用（未配置ADMIN_PASSWORD）",
        }));
    }
    let ip: Option<IpAddr> = req.peer_addr().map(|a| a.ip());
    // 原子登录：限流检查、凭证验证、失败计数/会话创建在同一次加锁内完成。
    // 旧实现"先检查后记录"分两次加锁，锁间隙不计数，并发突发可在任何失败
    // 被记录前全部通过检查，5次上限被放大；合并后各请求串行判定，上限精确生效
    let outcome =
        store
            .lock()
            .expect("session锁中毒")
            .login(ip, &config, &body.username, &body.password);
    match outcome {
        LoginOutcome::Locked => HttpResponse::TooManyRequests().json(serde_json::json!({
            "code": 429,
            "message": "失败次数过多，请稍后再试",
        })),
        LoginOutcome::BadCredentials => {
            tracing::warn!("登录失败 (user={:?}, ip={:?})", body.username, ip);
            HttpResponse::Unauthorized().json(serde_json::json!({
                "code": 401,
                "message": "用户名或密码错误",
            }))
        }
        LoginOutcome::Session(sid) => {
            tracing::info!("登录成功 (user={:?}, ip={:?})", body.username, ip);
            HttpResponse::Ok()
                .cookie(build_session_cookie(&sid, config.cookie_secure))
                .json(serde_json::json!({
                    "code": 200,
                    "message": "OK",
                    "data": { "username": body.username },
                }))
        }
    }
}

// POST /logout：移除服务端会话 + 下发过期cookie清除浏览器端
pub async fn logout(
    store: web::Data<Arc<Mutex<SessionStore>>>,
    config: web::Data<Arc<AuthConfig>>,
    req: HttpRequest,
) -> HttpResponse {
    if let Some(c) = req.cookie(SESSION_COOKIE) {
        store.lock().expect("session锁中毒").remove(c.value());
    }
    HttpResponse::Ok()
        .cookie(build_expired_cookie(config.cookie_secure))
        .json(serde_json::json!({ "code": 200, "message": "OK" }))
}
