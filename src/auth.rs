// API认证模块：env配置的管理员凭证 + 内存会话存储 + actix中间件
// 设计定位（自托管单管理员工具）：
//   - 凭证来自 ADMIN_USER/ADMIN_PASSWORD，未设置密码则认证关闭（启动时WARN提示）
//   - 会话仅内存态：进程重启需重新登录（登录成本极低，会话表无需持久化）
//   - 登录失败按IP限流：5次失败锁定60秒，防爆破
//   - /metrics 例外：可配 METRICS_TOKEN 走 Bearer 认证供Prometheus抓取，
//     未配置Token时metrics保持开放（导出器惯例），由部署者自行权衡

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use actix_web::cookie::time::Duration as CookieDuration;
use actix_web::cookie::{Cookie, SameSite};
use actix_web::dev::{Service, ServiceRequest, ServiceResponse, Transform};
use actix_web::http::Method;
use actix_web::{HttpRequest, HttpResponse};
use sha2::{Digest, Sha256};
use std::future::{Ready, ready};
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

// 限流兜底 key（ADR-0006 E3）：无法解析客户端 IP 时统一计入此固定桶，
// 不再"拿不到 IP 就跳过计数"——否则攻击者可借无源地址请求绕过失败锁定。
// 选用 0.0.0.0（未指定地址）：正常客户端不会以此为源地址，不会误伤真实用户。
const RATE_LIMIT_FALLBACK_IP: IpAddr = IpAddr::V4(Ipv4Addr::UNSPECIFIED);

// 认证配置（进程启动时从环境变量读取一次）
#[derive(Debug, Clone)]
pub struct AuthConfig {
    pub username: String,
    // 口令 argon2id PHC 字符串（$argon2id$...）：None 表示未配置口令（认证关闭）。
    // 由 ADMIN_PASSWORD_HASH 直接提供，或由明文 ADMIN_PASSWORD 启动时派生（见 from_env）
    password_hash: Option<String>,
    pub metrics_token: Option<String>,
    pub enabled: bool,
    // 会话cookie附加Secure标志；生产（非本机绑定）默认开启，见 from_env
    pub cookie_secure: bool,
    // 是否信任反向代理注入的 X-Forwarded-For（ADR-0006 E4）：
    // 开启后限流 key 优先取 XFF 最左客户端 IP；关闭时用传输层 peer_addr。
    // XFF 可被客户端伪造，仅在部署于可信反代之后时才应开启
    pub trust_proxy: bool,
}

// 判断绑定地址是否为本机回环（用于 Secure cookie 的生产默认推断）
fn is_local_bind(addr: &str) -> bool {
    // 去掉可能的端口部分，仅看主机名/IP。注意 IPv6 本身含冒号，不能简单按最后一个冒号切分：
    //   - [::1]:8080 / [::1] → 取中括号内的 ::1
    //   - 恰好一个冒号 → host:port（IPv4 或主机名），切掉端口
    //   - 其余（0 个或多个冒号）→ 视为裸 IPv6 或无端口主机名，整体即 host
    let host = if let Some(rest) = addr.strip_prefix('[') {
        rest.split(']').next().unwrap_or(rest)
    } else if addr.matches(':').count() == 1 {
        addr.rsplit_once(':').map(|(h, _)| h).unwrap_or(addr)
    } else {
        addr
    };
    matches!(host, "127.0.0.1" | "::1" | "localhost" | "")
}

// 读取布尔型环境变量（1/true/yes/on 视为真）
fn env_bool(key: &str) -> Option<bool> {
    std::env::var(key)
        .ok()
        .map(|v| matches!(v.to_lowercase().as_str(), "1" | "true" | "yes" | "on"))
}

impl AuthConfig {
    pub fn from_env() -> Self {
        let username = std::env::var("ADMIN_USER").unwrap_or_else(|_| "admin".to_string());
        // 口令摘要来源优先级：
        //   1) ADMIN_PASSWORD_HASH（推荐）：预先生成的 argon2 PHC 字符串，明文不进环境
        //   2) ADMIN_PASSWORD（兼容）：明文口令，启动时用随机盐派生一次 argon2 摘要
        let password_hash = match std::env::var("ADMIN_PASSWORD_HASH")
            .ok()
            .filter(|h| !h.trim().is_empty())
        {
            Some(h) => Some(h.trim().to_string()),
            None => {
                let password = std::env::var("ADMIN_PASSWORD").unwrap_or_default();
                if password.is_empty() {
                    None
                } else {
                    Some(hash_password(&password))
                }
            }
        };
        let enabled = password_hash.is_some();
        // 生产默认 Secure（ADR-0006 E7）：显式配置 COOKIE_SECURE 时以其为准；
        // 未配置时按绑定地址推断——非本机绑定（容器/公网）默认开启 Secure，
        // 本机调试（127.0.0.1）默认关闭（浏览器拒绝在 http 上发送 Secure cookie）
        let bind_addr = std::env::var("BIND_ADDR").unwrap_or_else(|_| "127.0.0.1".to_string());
        let cookie_secure = env_bool("COOKIE_SECURE").unwrap_or_else(|| !is_local_bind(&bind_addr));
        let trust_proxy = env_bool("TRUST_PROXY").unwrap_or(false);
        AuthConfig {
            username,
            password_hash,
            metrics_token: std::env::var("METRICS_TOKEN")
                .ok()
                .filter(|t| !t.is_empty()),
            enabled,
            cookie_secure,
            trust_proxy,
        }
    }

    // 凭证校验：用户名做常数时间摘要对比；口令走 argon2 PHC 校验（内部常数时间）
    pub fn verify(&self, username: &str, password: &str) -> bool {
        let Some(hash) = self.password_hash.as_ref() else {
            return false;
        };
        // 用户名摘要常数时间对比，避免"用户名对了才比口令"的分支时序差异
        let user_match: bool = sha256(username).ct_eq(&sha256(&self.username)).into();
        // 口令：解析 PHC 字符串后用 argon2 校验（verify_password 为常数时间比较）
        let pass_match = verify_password(hash, password);
        // 两个判定都已先行求值，再合并，避免短路带来的时序侧信道
        user_match && pass_match
    }
}

// 用随机盐派生 argon2id 口令摘要，返回 PHC 字符串。
// 盐取自 uuid v4 的 16 字节随机值（复用现有 uuid 依赖，免再引入 rand/getrandom）
fn hash_password(password: &str) -> String {
    use argon2::Argon2;
    use argon2::password_hash::{PasswordHasher, SaltString};
    let bytes = uuid::Uuid::new_v4().into_bytes();
    let salt = SaltString::encode_b64(&bytes).expect("argon2 盐编码失败");
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .expect("argon2 口令派生失败")
        .to_string()
}

// 校验明文口令是否匹配 argon2 PHC 字符串；PHC 非法时记录错误并判定失败
fn verify_password(phc: &str, password: &str) -> bool {
    use argon2::Argon2;
    use argon2::password_hash::{PasswordHash, PasswordVerifier};
    match PasswordHash::new(phc) {
        Ok(parsed) => Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok(),
        Err(e) => {
            tracing::error!("ADMIN_PASSWORD_HASH 不是合法的 argon2 PHC 字符串: {}", e);
            false
        }
    }
}

// 解析限流用客户端 IP（ADR-0006 E4）：trust_proxy 开启时优先取 X-Forwarded-For
// 最左侧客户端 IP（做基本的 IP 解析校验），否则回退传输层 peer_addr。
// 返回 None 表示彻底拿不到地址，由上层计入兜底桶（E3）
pub fn client_ip(req: &HttpRequest, trust_proxy: bool) -> Option<IpAddr> {
    if trust_proxy
        && let Some(xff) = req
            .headers()
            .get("X-Forwarded-For")
            .and_then(|v| v.to_str().ok())
        && let Some(first) = xff.split(',').next()
        && let Ok(ip) = first.trim().parse::<IpAddr>()
    {
        return Some(ip);
    }
    req.peer_addr().map(|a| a.ip())
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
        // 拿不到客户端 IP 时计入固定兜底桶，不跳过计数（ADR-0006 E3，防限流绕过）
        let key = ip.unwrap_or(RATE_LIMIT_FALLBACK_IP);
        // 锁定截止时间未到：直接拒绝，不做凭证比较
        if matches!(self.attempts.get(&key), Some((_, _, until)) if *until > now) {
            return LoginOutcome::Locked;
        }
        if !config.verify(username, password) {
            let entry = self.attempts.entry(key).or_insert((0, now, 0));
            entry.0 += 1;
            entry.1 = now;
            if entry.0 >= MAX_FAILURES {
                // 锁定窗口重置计数：锁定结束后需重新累积5次
                *entry = (0, now, now + LOCKOUT_SECS);
            }
            return LoginOutcome::BadCredentials;
        }
        // 登录成功清零失败计数并建立会话
        self.attempts.remove(&key);
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
        // 写请求同源校验（ADR-0006 E7，CSRF 防护）：带 Origin/Referer 时其 host 必须
        // 与请求 Host 一致，不一致直接 403。两者都缺省时放行（非浏览器客户端如 curl）。
        // 置于最前：登录/登出等放行路径的写请求也一并受保护
        if is_write_method(req.method()) && !same_origin(req) {
            return Some(forbidden_response());
        }
        let path = req.path();
        // 未启用认证：全部放行（启动时已WARN提示）
        if !self.config.enabled {
            return None;
        }
        // 登录接口放行（限流在handler内做）
        if path == "/login" && req.method() == Method::POST {
            return None;
        }
        // 健康探针放行：存活检查无敏感信息，需对编排器/负载均衡免认证开放
        if path == "/healthz" && req.method() == Method::GET {
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
        // 其余：会话cookie（锁中毒时从毒化锁恢复而非级联 panic，ADR-0006 E2）
        match req.cookie(SESSION_COOKIE) {
            Some(c)
                if self
                    .store
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .validate(c.value()) =>
            {
                None
            }
            _ => Some(unauthorized_response()),
        }
    }
}

// 状态变更方法（需 CSRF 同源校验）
fn is_write_method(method: &Method) -> bool {
    matches!(
        *method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    )
}

// 写请求同源校验（CSRF 防护）：若带 Origin 或 Referer，其 authority(host[:port])
// 必须与请求 Host 一致；两者都缺省时放行（非浏览器客户端不发这些头）。
// 浏览器对跨站写请求会带上 Origin，据此阻断 CSRF；同源的 fetch 不受影响
fn same_origin(req: &ServiceRequest) -> bool {
    let conn = req.connection_info();
    let host = conn.host().to_string();
    // 从 Origin/Referer 的 URL 取 authority（scheme://authority/...）
    let authority_of = |val: &str| -> Option<String> {
        let after_scheme = val.split("://").nth(1)?;
        Some(after_scheme.split('/').next().unwrap_or("").to_string())
    };
    let headers = req.headers();
    if let Some(origin) = headers.get("Origin").and_then(|v| v.to_str().ok()) {
        // "null" 等不含 scheme:// 的 Origin 一律判为不同源
        return authority_of(origin).map(|a| a == host).unwrap_or(false);
    }
    if let Some(referer) = headers.get("Referer").and_then(|v| v.to_str().ok()) {
        return authority_of(referer).map(|a| a == host).unwrap_or(false);
    }
    true
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

// 403响应体：写请求同源校验失败（CSRF 防护拦截）
pub fn forbidden_response() -> HttpResponse {
    HttpResponse::Forbidden().json(serde_json::json!({
        "code": 403,
        "message": "跨站请求被拒绝（Origin/Referer 校验未通过）",
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
        ATTEMPT_IDLE_SECS, Auth, AuthConfig, LoginOutcome, MAX_TRACKED_IPS, SESSION_COOKIE,
        SessionStore,
    };
    use std::net::IpAddr;
    use std::sync::{Arc, Mutex};

    fn config(enabled: bool) -> Arc<AuthConfig> {
        Arc::new(AuthConfig {
            username: "admin".to_string(),
            // 启用时放入 s3cret 的 argon2 摘要；禁用时 None（verify 一律拒绝）
            password_hash: if enabled {
                Some(super::hash_password("s3cret"))
            } else {
                None
            },
            metrics_token: None,
            enabled,
            cookie_secure: false,
            trust_proxy: false,
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
    fn argon2_hash_roundtrips_and_rejects_wrong_password() {
        // 口令 KDF 升级为 argon2id（ADR-0006 E6）：同口令两次派生摘要不同（随机盐），
        // 但都应能校验通过；错误口令校验失败
        let h1 = super::hash_password("s3cret");
        let h2 = super::hash_password("s3cret");
        assert!(h1.starts_with("$argon2"), "应为 argon2 PHC 字符串: {h1}");
        assert_ne!(h1, h2, "随机盐应使同口令摘要不同");
        assert!(super::verify_password(&h1, "s3cret"));
        assert!(super::verify_password(&h2, "s3cret"));
        assert!(!super::verify_password(&h1, "wrong"));
        // 非法 PHC 字符串判定失败而非 panic
        assert!(!super::verify_password("not-a-phc", "s3cret"));
    }

    #[test]
    fn login_without_ip_counts_toward_fallback_bucket() {
        // E3：拿不到 IP（None）时计入固定兜底桶，仍能触发锁定（防限流绕过）
        let mut store = SessionStore::new();
        let cfg = config(true);
        for _ in 0..MAX_FAILURES_TEST {
            let _ = store.login(None, &cfg, "admin", "wrong");
        }
        assert!(
            matches!(store.login(None, &cfg, "admin", "s3cret"), LoginOutcome::Locked),
            "无 IP 的连续失败应计入兜底桶并最终锁定"
        );
    }

    const MAX_FAILURES_TEST: u32 = 5;

    #[test]
    fn is_local_bind_detects_loopback() {
        assert!(super::is_local_bind("127.0.0.1"));
        assert!(super::is_local_bind("127.0.0.1:8080"));
        assert!(super::is_local_bind("localhost"));
        assert!(super::is_local_bind("::1"));
        assert!(!super::is_local_bind("0.0.0.0"));
        assert!(!super::is_local_bind("10.0.0.5:8080"));
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
            matches!(
                store.login(ip, &cfg, "admin", "wrong"),
                LoginOutcome::Locked
            ),
            "5次失败后应锁定"
        );
        assert!(
            matches!(
                store.login(ip, &cfg, "admin", "s3cret"),
                LoginOutcome::Locked
            ),
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
        // 直接向 attempts 注入超上限的"近期失败"条目来模拟 IPv6 轮换 flood。
        // 不经 login()：口令 KDF 已升级为 argon2（刻意很慢），逐 IP 跑一次校验会让
        // 本用例耗时数分钟；而本用例只验证 sweep 的容量上限驱逐分支，直接构造状态即可。
        let now = chrono::Utc::now().timestamp();
        for i in 0..(MAX_TRACKED_IPS as u32 + 100) {
            let ip = IpAddr::from([10u8, 0, (i / 256) as u8, (i % 256) as u8]);
            // (失败计数>0, 最近活动=now, 未锁定)：先过 retain 的保留条件，再触发容量上限驱逐
            store.attempts.insert(ip, (1, now, 0));
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
        assert_eq!(
            expired.max_age(),
            Some(actix_web::cookie::time::Duration::ZERO)
        );
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
                .route("/api/protected", web::get().to(|| async { "secret-data" })),
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
    async fn healthz_is_allowed_without_session() {
        use actix_web::test;
        use actix_web::web;

        let store = Arc::new(Mutex::new(SessionStore::new()));
        let app = test::init_service(
            actix_web::App::new()
                .wrap(Auth::new(store, config(true)))
                .route("/healthz", web::get().to(|| async { "ok" })),
        )
        .await;
        // 认证已启用、无cookie：/healthz 仍应放行（编排器探活免认证）
        let req = test::TestRequest::get().uri("/healthz").to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), 200);
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
            password_hash: Some(super::hash_password("s3cret")),
            metrics_token: Some("tok-123".to_string()),
            enabled: true,
            cookie_secure: false,
            trust_proxy: false,
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
