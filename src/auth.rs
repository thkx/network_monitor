// API认证模块：env配置的管理员凭证 + 内存会话存储 + actix中间件
// 设计定位（自托管单管理员工具）：
//   - 凭证来自 ADMIN_USER/ADMIN_PASSWORD，未设置密码则认证关闭（启动时WARN提示）
//   - 会话仅内存态：进程重启需重新登录（登录成本极低，会话表无需持久化）
//   - 登录失败按IP限流：5次失败锁定60秒，防爆破
//   - /metrics 例外：可配 METRICS_TOKEN 走 Bearer 认证供Prometheus抓取，
//     未配置Token时metrics保持开放（导出器惯例），由部署者自行权衡

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use actix_web::cookie::time::Duration as CookieDuration;
use actix_web::cookie::{Cookie, SameSite};
use actix_web::dev::{Service, ServiceRequest, ServiceResponse, Transform};
use actix_web::http::Method;
use actix_web::HttpResponse;
use sha2::{Digest, Sha256};
use std::future::{ready, Ready};
use subtle::ConstantTimeEq;

// 本地 boxed future别名：actix-web worker是单线程运行时，!Send future可用
// （与actix_service的LocalBoxFuture同义，避免为此引入futures-util直接依赖）
type BoxFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + 'a>>;

// 会话有效期（12小时）
pub const SESSION_TTL_SECS: i64 = 12 * 60 * 60;
pub const SESSION_COOKIE: &str = "monitor_session";
// 登录限流：5次失败锁60秒
const MAX_FAILURES: u32 = 5;
const LOCKOUT_SECS: i64 = 60;
// 失败记录保留窗口：未锁定且最后活动超过该时长的IP条目可被清扫
const ATTEMPT_IDLE_SECS: i64 = 600;
// 限流追踪的IP容量上限：防IPv6轮换等 flood 把 attempts 撑爆内存
const MAX_TRACKED_IPS: usize = 10_000;

// 认证配置（进程启动时从环境变量读取一次）
#[derive(Debug, Clone)]
pub struct AuthConfig {
    pub username: String,
    // 存储SHA-256摘要：内存中不保留明文，比较走常数时间摘要对比
    pub password_digest: [u8; 32],
    pub metrics_token: Option<String>,
    pub enabled: bool,
    // 会话cookie附加Secure标志（COOKIE_SECURE=1/true，HTTPS部署时开启）
    pub cookie_secure: bool,
}

impl AuthConfig {
    pub fn from_env() -> Self {
        let username = std::env::var("ADMIN_USER").unwrap_or_else(|_| "admin".to_string());
        let password = std::env::var("ADMIN_PASSWORD").unwrap_or_default();
        let enabled = !password.is_empty();
        let cookie_secure = std::env::var("COOKIE_SECURE")
            .map(|v| matches!(v.to_lowercase().as_str(), "1" | "true" | "yes" | "on"))
            .unwrap_or(false);
        AuthConfig {
            username,
            password_digest: sha256(&password),
            metrics_token: std::env::var("METRICS_TOKEN")
                .ok()
                .filter(|t| !t.is_empty()),
            enabled,
            cookie_secure,
        }
    }

    // 凭证校验：用户名与密码摘要均做常数时间对比，避免时序侧信道
    pub fn verify(&self, username: &str, password: &str) -> bool {
        if !self.enabled {
            return false;
        }
        // [u8;32]的==是可短路的逐元素比较，并非保证常数时间；摘要对比用subtle的ct_eq
        let user_match: bool = sha256(username).ct_eq(&sha256(&self.username)).into();
        let pass_match: bool = sha256(password).ct_eq(&self.password_digest).into();
        // 两个比较都执行，避免"用户名对了才比密码"的分支时序差异
        user_match && pass_match
    }
}

// 会话存储：session_id -> (用户名, 过期时间unix秒)
#[derive(Debug, Default)]
pub struct SessionStore {
    sessions: HashMap<String, (String, i64)>,
    // 登录失败限流：ip -> (连续失败次数, 最后活动unix秒, 锁定截止unix秒)
    attempts: HashMap<IpAddr, (u32, i64, i64)>,
}

// 单次登录尝试的原子结果
pub enum LoginOutcome {
    // 锁定中：不再验证凭证，调用方回429
    Locked,
    // 凭证错误：已计入失败，调用方回401
    BadCredentials,
    // 成功：携带新会话id
    Session(String),
}

impl SessionStore {
    pub fn new() -> Self {
        SessionStore::default()
    }

    fn now() -> i64 {
        chrono::Utc::now().timestamp()
    }

    // 创建会话，返回session_id（uuid v4：122位随机度）
    pub fn create(&mut self, username: &str) -> String {
        let sid = uuid::Uuid::new_v4().to_string();
        self.sessions.insert(
            sid.clone(),
            (username.to_string(), Self::now() + SESSION_TTL_SECS),
        );
        sid
    }

    // 校验会话：存在且未过期（过期惰性清理）
    pub fn validate(&mut self, sid: &str) -> bool {
        let now = Self::now();
        match self.sessions.get(sid) {
            Some((_, expires)) if *expires > now => true,
            Some(_) => {
                self.sessions.remove(sid);
                false
            }
            None => false,
        }
    }

    pub fn remove(&mut self, sid: &str) {
        self.sessions.remove(sid);
    }

    // 原子登录：限流检查、凭证验证、失败计数/会话创建在同一次加锁内完成。
    // 旧实现"先检查(allow_attempt)后记录(record_failure)"分两次加锁，两者之间
    // 不持锁，并发突发可在任何失败被计数前全部通过检查，5次上限被放大；
    // 合并后各请求在互斥锁上串行化，上限对突发流量精确生效
    pub fn login(
        &mut self,
        ip: Option<IpAddr>,
        config: &AuthConfig,
        username: &str,
        password: &str,
    ) -> LoginOutcome {
        let now = Self::now();
        // 锁定截止时间未到：直接拒绝，不做凭证比较
        if let Some(ip) = ip
            && matches!(self.attempts.get(&ip), Some((_, _, until)) if *until > now)
        {
            return LoginOutcome::Locked;
        }
        if !config.verify(username, password) {
            if let Some(ip) = ip {
                let entry = self.attempts.entry(ip).or_insert((0, now, 0));
                entry.0 += 1;
                entry.1 = now;
                if entry.0 >= MAX_FAILURES {
                    // 锁定窗口重置计数：锁定结束后需重新累积5次
                    *entry = (0, now, now + LOCKOUT_SECS);
                }
            }
            return LoginOutcome::BadCredentials;
        }
        // 登录成功清零失败计数并建立会话
        if let Some(ip) = ip {
            self.attempts.remove(&ip);
        }
        LoginOutcome::Session(self.create(username))
    }

    // 周期清扫：过期会话 + 过期失败记录，防两个HashMap无界增长
    // 由run_server的后台任务周期调用；validate的惰性清理仍保留（覆盖刚过期的热会话）
    pub fn sweep(&mut self) {
        let now = Self::now();
        self.sessions.retain(|_, (_, expires)| *expires > now);
        // 保留：锁定中（until未到）或仍有失败计数且最近有活动的条目
        self.attempts.retain(|_, (fails, last, until)| {
            *until > now || (*fails > 0 && now - *last < ATTEMPT_IDLE_SECS)
        });
        // 兜底容量上限：仍超限（如持续攻击制造海量新IP）时近似按最旧活动驱逐
        if self.attempts.len() > MAX_TRACKED_IPS {
            let excess = self.attempts.len() - MAX_TRACKED_IPS;
            let mut by_last: Vec<(IpAddr, i64)> = self
                .attempts
                .iter()
                .map(|(ip, (_, last, _))| (*ip, *last))
                .collect();
            by_last.sort_unstable_by_key(|(_, last)| *last);
            for (ip, _) in by_last.into_iter().take(excess) {
                self.attempts.remove(&ip);
            }
        }
    }
}

// 认证中间件：静态页与登录接口放行，其余要求有效会话
// /metrics：配置了METRICS_TOKEN时接受Bearer，未配置时开放（导出器惯例）
pub struct Auth {
    store: Arc<Mutex<SessionStore>>,
    config: Arc<AuthConfig>,
}

impl Auth {
    pub fn new(store: Arc<Mutex<SessionStore>>, config: Arc<AuthConfig>) -> Self {
        Auth { store, config }
    }

    // 请求放行判定：Some(reject)=拒绝响应；None=放行
    pub fn check(&self, req: &ServiceRequest) -> Option<HttpResponse> {
        let path = req.path();
        // 未启用认证：全部放行（启动时已WARN提示）
        if !self.config.enabled {
            return None;
        }
        // 登录接口放行（限流在handler内做）
        if path == "/login" && req.method() == Method::POST {
            return None;
        }
        // 控制台静态页放行：页面本身无敏感数据，401由页面JS请求/api/*时触发，
        // 弹出登录浮层（否则HTML加载不出来，用户只能看到裸401 JSON）
        if path == "/" && req.method() == Method::GET {
            return None;
        }
        // /metrics：未配置Token时开放（`?`在None时直接放行，导出器惯例）；
        // 配置了Token时接受正确Bearer
        if path == "/metrics" {
            let expected = self.config.metrics_token.as_ref()?;
            if let Some(token) = bearer_token(req)
                && bool::from(sha256(&token).ct_eq(&sha256(expected)))
            {
                return None;
            }
        }
        // 其余：会话cookie
        match req.cookie(SESSION_COOKIE) {
            Some(c) if self.store.lock().expect("session锁中毒").validate(c.value()) => None,
            _ => Some(unauthorized_response()),
        }
    }
}

// 从Authorization头提取Bearer token
fn bearer_token(req: &ServiceRequest) -> Option<String> {
    let header = req.headers().get("Authorization")?.to_str().ok()?;
    header
        .strip_prefix("Bearer ")
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

// SHA-256摘要
fn sha256(data: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(data.as_bytes());
    hasher.finalize().into()
}

// 供handler构造会话cookie；secure来自COOKIE_SECURE（HTTPS部署时开启）
pub fn build_session_cookie(sid: &str, secure: bool) -> Cookie<'static> {
    Cookie::build(SESSION_COOKIE, sid.to_string())
        .path("/")
        .http_only(true)
        .same_site(SameSite::Lax)
        .max_age(CookieDuration::seconds(SESSION_TTL_SECS))
        .secure(secure)
        .finish()
}

// 供handler构造清除cookie（登出）；属性需与下发时一致才能可靠删除
pub fn build_expired_cookie(secure: bool) -> Cookie<'static> {
    Cookie::build(SESSION_COOKIE, "")
        .path("/")
        .http_only(true)
        .same_site(SameSite::Lax)
        .max_age(CookieDuration::ZERO)
        .secure(secure)
        .finish()
}

// 401响应体统一形状（前端据此弹出登录浮层）
pub fn unauthorized_response() -> HttpResponse {
    HttpResponse::Unauthorized().json(serde_json::json!({
        "code": 401,
        "message": "未登录或会话已过期",
    }))
}

// ---- actix中间件 ----

impl<S> Transform<S, ServiceRequest> for Auth
where
    S: Service<ServiceRequest, Response = ServiceResponse, Error = actix_web::Error> + 'static,
    S::Future: 'static,
{
    type Response = ServiceResponse;
    type Error = actix_web::Error;
    type Transform = AuthMiddleware<S>;
    type InitError = ();
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        ready(Ok(AuthMiddleware {
            service,
            auth: Auth::new(self.store.clone(), self.config.clone()),
        }))
    }
}

pub struct AuthMiddleware<S> {
    service: S,
    auth: Auth,
}

impl<S> Service<ServiceRequest> for AuthMiddleware<S>
where
    S: Service<ServiceRequest, Response = ServiceResponse, Error = actix_web::Error> + 'static,
    S::Future: 'static,
{
    type Response = ServiceResponse;
    type Error = actix_web::Error;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&self, ctx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.service.poll_ready(ctx)
    }

    fn call(&self, req: ServiceRequest) -> Self::Future {
        // 拒绝时构造401直接返回，不执行下游业务handler
        if let Some(reject) = self.auth.check(&req) {
            return Box::pin(async move { Ok(req.into_response(reject)) });
        }
        let future = self.service.call(req);
        Box::pin(future)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Auth, AuthConfig, LoginOutcome, SessionStore, SESSION_COOKIE, ATTEMPT_IDLE_SECS,
        MAX_TRACKED_IPS,
    };
    use std::net::IpAddr;
    use std::sync::{Arc, Mutex};

    fn config(enabled: bool) -> Arc<AuthConfig> {
        Arc::new(AuthConfig {
            username: "admin".to_string(),
            password_digest: super::sha256("s3cret"),
            metrics_token: None,
            enabled,
            cookie_secure: false,
        })
    }

    #[test]
    fn verify_accepts_only_correct_credentials() {
        let cfg = config(true);
        assert!(cfg.verify("admin", "s3cret"));
        assert!(!cfg.verify("admin", "wrong"));
        assert!(!cfg.verify("root", "s3cret"));
        // 未启用时一律拒绝
        assert!(!config(false).verify("admin", "s3cret"));
    }

    #[test]
    fn session_create_validate_and_expire() {
        let mut store = SessionStore::new();
        let sid = store.create("admin");
        assert!(store.validate(&sid));
        // 未知会话
        assert!(!store.validate("no-such-id"));
        // 过期：直接把过期时间改到过去
        let now = chrono::Utc::now().timestamp();
        store
            .sessions
            .insert(sid.clone(), ("admin".to_string(), now - 1));
        assert!(!store.validate(&sid), "过期会话应判无效");
        assert!(!store.sessions.contains_key(&sid), "过期会话应被惰性清理");
        // 登出移除
        let sid2 = store.create("admin");
        store.remove(&sid2);
        assert!(!store.validate(&sid2));
    }

    #[test]
    fn login_lockout_after_max_failures() {
        let mut store = SessionStore::new();
        let cfg = config(true);
        let ip = Some("192.168.1.9".parse().unwrap());
        // 5次错误凭证：均返回BadCredentials并计数
        for i in 0..5 {
            assert!(
                matches!(
                    store.login(ip, &cfg, "admin", "wrong"),
                    LoginOutcome::BadCredentials
                ),
                "第{}次错误凭证应为BadCredentials",
                i + 1
            );
        }
        // 第6次起锁定：即使凭证正确也在验证前拒绝（防爆破语义）
        assert!(
            matches!(store.login(ip, &cfg, "admin", "wrong"), LoginOutcome::Locked),
            "5次失败后应锁定"
        );
        assert!(
            matches!(store.login(ip, &cfg, "admin", "s3cret"), LoginOutcome::Locked),
            "锁定期间正确凭证也应拒绝"
        );
        // 不同IP互不影响
        let other = Some("10.0.0.1".parse().unwrap());
        assert!(matches!(
            store.login(other, &cfg, "admin", "s3cret"),
            LoginOutcome::Session(_)
        ));
    }

    #[test]
    fn successful_login_clears_failure_counter() {
        let mut store = SessionStore::new();
        let cfg = config(true);
        let ip = Some("192.168.1.9".parse().unwrap());
        for _ in 0..4 {
            let _ = store.login(ip, &cfg, "admin", "wrong");
        }
        // 第5次直接成功：计数清零，不触发锁定
        assert!(matches!(
            store.login(ip, &cfg, "admin", "s3cret"),
            LoginOutcome::Session(_)
        ));
        // 清零后重新累积，不受旧计数波及
        for _ in 0..4 {
            let _ = store.login(ip, &cfg, "admin", "wrong");
        }
        assert!(matches!(
            store.login(ip, &cfg, "admin", "s3cret"),
            LoginOutcome::Session(_)
        ));
    }

    #[test]
    fn sweep_removes_expired_sessions_and_stale_attempts() {
        let mut store = SessionStore::new();
        let cfg = config(true);
        // 未过期会话：sweep保留
        let sid = store.create("admin");
        store.sweep();
        assert!(store.validate(&sid));
        // 已过期会话：sweep清理
        let now = chrono::Utc::now().timestamp();
        store
            .sessions
            .insert(sid.clone(), ("admin".to_string(), now - 1));
        store.sweep();
        assert!(!store.sessions.contains_key(&sid), "过期会话应被清扫");
        // 锁定中的IP：sweep保留
        let locked: IpAddr = "10.0.0.1".parse().unwrap();
        for _ in 0..5 {
            let _ = store.login(Some(locked), &cfg, "admin", "wrong");
        }
        store.sweep();
        assert!(
            store.attempts.contains_key(&locked),
            "锁定中的条目不应被清扫"
        );
        // 有少量失败、但最后活动已超过保留窗口的IP：sweep清理
        let idle: IpAddr = "10.0.0.2".parse().unwrap();
        let _ = store.login(Some(idle), &cfg, "admin", "wrong");
        store.attempts.get_mut(&idle).unwrap().1 = now - ATTEMPT_IDLE_SECS - 1;
        store.sweep();
        assert!(
            !store.attempts.contains_key(&idle),
            "过期且未锁定的失败记录应被清扫"
        );
    }

    #[test]
    fn sweep_caps_tracked_ips_under_flood() {
        let mut store = SessionStore::new();
        let cfg = config(true);
        // 制造超上限的失败条目（模拟IPv6轮换flood）
        for i in 0..(MAX_TRACKED_IPS as u32 + 100) {
            let ip = IpAddr::from([10u8, 0, (i / 256) as u8, (i % 256) as u8]);
            let _ = store.login(Some(ip), &cfg, "admin", "wrong");
        }
        store.sweep();
        assert!(
            store.attempts.len() <= MAX_TRACKED_IPS,
            "清扫后条目数应回到上限内: {}",
            store.attempts.len()
        );
    }

    #[test]
    fn session_cookie_honors_secure_flag() {
        let secure = super::build_session_cookie("abc", true);
        assert_eq!(secure.secure(), Some(true));
        assert_eq!(secure.http_only(), Some(true));
        let plain = super::build_session_cookie("abc", false);
        assert_eq!(plain.secure(), Some(false));
        let expired = super::build_expired_cookie(true);
        assert_eq!(expired.secure(), Some(true));
        assert_eq!(expired.max_age(), Some(actix_web::cookie::time::Duration::ZERO));
    }

    #[actix_web::test]
    async fn middleware_rejects_without_session_and_allows_with() {
        use actix_web::cookie::Cookie;
        use actix_web::test;
        use actix_web::web;

        let store = Arc::new(Mutex::new(SessionStore::new()));
        let cfg = config(true);
        let app = test::init_service(
            actix_web::App::new()
                .wrap(Auth::new(store.clone(), cfg))
                .route(
                    "/api/protected",
                    web::get().to(|| async { "secret-data" }),
                ),
        )
        .await;

        // 无cookie：401
        let req = test::TestRequest::get().uri("/api/protected").to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), 401);

        // 有效会话cookie：200 + 业务响应
        let sid = store.lock().unwrap().create("admin");
        let req = test::TestRequest::get()
            .uri("/api/protected")
            .cookie(Cookie::build(SESSION_COOKIE, sid).finish())
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), 200);
        let body = test::read_body(resp).await;
        assert_eq!(&body[..], b"secret-data");
    }

    #[actix_web::test]
    async fn console_page_is_served_without_session() {
        use actix_web::test;
        use actix_web::web;

        let store = Arc::new(Mutex::new(SessionStore::new()));
        let app = test::init_service(
            actix_web::App::new()
                .wrap(Auth::new(store, config(true)))
                .route("/", web::get().to(|| async { "<html>console</html>" })),
        )
        .await;
        // 无cookie：控制台页放行（登录浮层由页面JS对/api/*的401触发）
        let req = test::TestRequest::get().uri("/").to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), 200);
        // 登录接口放行后，其余API仍受保护
        let req = test::TestRequest::get().uri("/api/monitors").to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), 401);
    }

    #[actix_web::test]
    async fn disabled_auth_allows_everything() {
        use actix_web::test;
        use actix_web::web;

        let store = Arc::new(Mutex::new(SessionStore::new()));
        let cfg = config(false);
        let app = test::init_service(
            actix_web::App::new()
                .wrap(Auth::new(store, cfg))
                .route("/api/protected", web::get().to(|| async { "open" })),
        )
        .await;
        let req = test::TestRequest::get().uri("/api/protected").to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), 200);
    }

    #[actix_web::test]
    async fn metrics_bearer_token_is_accepted() {
        use actix_web::test;
        use actix_web::web;

        let store = Arc::new(Mutex::new(SessionStore::new()));
        let cfg = Arc::new(AuthConfig {
            username: "admin".to_string(),
            password_digest: super::sha256("s3cret"),
            metrics_token: Some("tok-123".to_string()),
            enabled: true,
            cookie_secure: false,
        });
        let app = test::init_service(
            actix_web::App::new()
                .wrap(Auth::new(store, cfg))
                .route("/metrics", web::get().to(|| async { "metrics" })),
        )
        .await;
        // 无凭证：401
        let req = test::TestRequest::get().uri("/metrics").to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), 401);
        // 错误Token：401
        let req = test::TestRequest::get()
            .uri("/metrics")
            .insert_header(("Authorization", "Bearer wrong"))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), 401);
        // 正确Bearer：200
        let req = test::TestRequest::get()
            .uri("/metrics")
            .insert_header(("Authorization", "Bearer tok-123"))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), 200);
    }
}
