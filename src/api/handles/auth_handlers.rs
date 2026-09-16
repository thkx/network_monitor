// 登录/登出处理器：凭证验证、限流、会话cookie下发与清除
use std::net::IpAddr;
use std::sync::{Arc, Mutex};

use actix_web::{web, HttpRequest, HttpResponse};
use serde::Deserialize;

use crate::auth::{build_expired_cookie, build_session_cookie, AuthConfig, SessionStore};

#[derive(Debug, Deserialize)]
pub struct LoginBody {
    pub username: String,
    pub password: String,
}

// POST /login：验证凭证 -> 下发HttpOnly会话cookie
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
    // 限流前置：锁定中的IP直接429，不做凭证比较
    {
        let mut s = store.lock().expect("session锁中毒");
        if !s.allow_attempt(ip) {
            return HttpResponse::TooManyRequests().json(serde_json::json!({
                "code": 429,
                "message": "失败次数过多，请稍后再试",
            }));
        }
    }
    if !config.verify(&body.username, &body.password) {
        store.lock().expect("session锁中毒").record_failure(ip);
        tracing::warn!("登录失败 (user={:?}, ip={:?})", body.username, ip);
        return HttpResponse::Unauthorized().json(serde_json::json!({
            "code": 401,
            "message": "用户名或密码错误",
        }));
    }
    let sid = {
        let mut s = store.lock().expect("session锁中毒");
        s.clear_failures(ip);
        s.create(&body.username)
    };
    tracing::info!("登录成功 (user={:?}, ip={:?})", body.username, ip);
    HttpResponse::Ok()
        .cookie(build_session_cookie(&sid))
        .json(
            serde_json::json!({ "code": 200, "message": "OK", "data": { "username": body.username } }),
        )
}

// POST /logout：移除服务端会话 + 下发过期cookie清除浏览器端
pub async fn logout(
    store: web::Data<Arc<Mutex<SessionStore>>>,
    req: HttpRequest,
) -> HttpResponse {
    if let Some(c) = req.cookie(crate::auth::SESSION_COOKIE) {
        store.lock().expect("session锁中毒").remove(c.value());
    }
    HttpResponse::Ok()
        .cookie(build_expired_cookie())
        .json(serde_json::json!({ "code": 200, "message": "OK" }))
}
