// 监控配置的 CRUD、启停与手动执行 handler。
// 通用响应类型见 response，配置校验见 validation，控制台聚合视图见 status_handlers。
use actix_web::{HttpResponse, web};
use std::sync::{Arc, Mutex};

use crate::async_monitor::{AsyncMonitor, ResultRoute};
use crate::database::models::{MonitorConfigInsert, MonitorConfigUpdate};
use crate::database::services::build_monitor_insert;
use crate::database::services::monitor_service::MonitorService;
use crate::database::services::result_service::ResultService;
use crate::domain::MonitorDefinition;
use crate::metrics::MetricsRegistry;
use crate::monitor::MonitorFactory;
use crate::scheduler::Scheduler;

use super::response::{
    DefaultResponseObj, PageData, PaginationParams, clamp_pagination, internal_error, run_blocking,
};

// 读接口输出脱敏（ADR-0006 E5）：把配置里 notify_config 的 password/secret 等
// 敏感字段替换为掩码，仅改读接口返回体，不动落库值。
// config_json 是序列化后的 MonitorDefinition 字符串，这里反序列化为通用 JSON、
// 递归按字段名掩码后再序列化回去——对嵌套结构（notify_config.secret /
// notify_config.email.password）都生效，且不依赖具体字段路径，后续新增渠道也兜得住。
const SENSITIVE_KEYS: [&str; 2] = ["password", "secret"];

fn mask_sensitive_config(model: &mut crate::database::models::MonitorConfigModel) {
    let Some(raw) = model.config_json.as_ref() else {
        return;
    };
    // 解析失败（脏配置）时保持原样：脱敏是尽力而为，不因此让读接口报错
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return;
    };
    mask_secrets_in_value(&mut value);
    if let Ok(s) = serde_json::to_string(&value) {
        model.config_json = Some(s);
    }
}

// 递归扫描 JSON：对名为 password/secret 的非空字段统一掩码为 "***"。
// 空字符串保持原样（避免把"未配置"误显示成"已配置了某个值"）
fn mask_secrets_in_value(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            for (k, v) in map.iter_mut() {
                if SENSITIVE_KEYS.iter().any(|s| k.eq_ignore_ascii_case(s)) {
                    match v {
                        serde_json::Value::Null => {}
                        serde_json::Value::String(s) if s.is_empty() => {}
                        _ => *v = serde_json::Value::String("***".to_string()),
                    }
                } else {
                    mask_secrets_in_value(v);
                }
            }
        }
        serde_json::Value::Array(arr) => {
            for item in arr.iter_mut() {
                mask_secrets_in_value(item);
            }
        }
        _ => {}
    }
}

// 获取全部的监控配置处理函数
pub async fn get_all_monitors(
    monitor_service: web::Data<MonitorService>, // 设置统一的业务层的对象，进行相关的数据操作
    query: web::Query<PaginationParams>,        // 分页参数对象
) -> Result<HttpResponse, actix_web::Error> {
    let (no, size) = clamp_pagination(query.page_no, query.page_size);
    // enabled 可选筛选启停；tag 可选按分组标签筛选（空串视作不筛选）
    let tag = query
        .tag
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let enabled = query.enabled;
    // 同步 diesel 查询挪阻塞池（ADR-0002 A1），错误脱敏为通用 500（E1）
    let svc = monitor_service.get_ref().clone();
    let (mut monitors, total) =
        run_blocking(move || svc.list_monitors(enabled, tag.as_deref(), no, size)).await?;
    // 列表返回路径同样脱敏 notify_config 中的 password/secret（E5）
    for m in monitors.iter_mut() {
        mask_sensitive_config(m);
    }
    Ok(HttpResponse::Ok().json(DefaultResponseObj {
        code: 200,
        message: "OK".to_string(),
        data: PageData {
            list: monitors,
            total,
            page_no: no,
            page_size: size,
        },
    }))
}

// GET /api/monitors/tags：库中出现过的全部分组标签（去重升序），供列表页筛选下拉
pub async fn get_monitor_tags(
    monitor_service: web::Data<MonitorService>,
) -> Result<HttpResponse, actix_web::Error> {
    // 同步 diesel 查询挪阻塞池（ADR-0002 A1）
    let svc = monitor_service.get_ref().clone();
    let tags = run_blocking(move || svc.get_distinct_tags()).await?;
    Ok(HttpResponse::Ok().json(DefaultResponseObj {
        code: 200,
        message: "OK".to_string(),
        data: tags,
    }))
}

// 按ID获取监控配置详情
pub async fn get_monitor_by_id(
    monitor_service: web::Data<MonitorService>,
    path: web::Path<i32>,
) -> Result<HttpResponse, actix_web::Error> {
    let id = path.into_inner();
    // 同步 diesel 查询挪阻塞池（ADR-0002 A1）
    let svc = monitor_service.get_ref().clone();
    let monitor = run_blocking(move || svc.get_monitor_by_id(id)).await?;
    match monitor {
        Some(mut m) => {
            // 读接口输出脱敏：notify_config 的 password/secret 返回掩码（E5）
            mask_sensitive_config(&mut m);
            Ok(HttpResponse::Ok().json(DefaultResponseObj {
                code: 200,
                message: "OK".to_string(),
                data: m,
            }))
        }
        None => Err(actix_web::error::ErrorNotFound(format!(
            "Monitor {} not found",
            id
        ))),
    }
}

// 热更新重建：reload_all内含同步diesel查询（从库加载启用配置），
// 统一挪到actix阻塞线程池执行，避免SQLite读写在worker线程上卡顿
async fn reload_scheduler(
    scheduler: &web::Data<Arc<Mutex<Scheduler>>>,
    monitor_service: &MonitorService,
) {
    let sched = Arc::clone(scheduler);
    let svc = monitor_service.clone();
    let _ = web::block(move || {
        if let Ok(mut s) = sched.lock() {
            s.reload_all(&svc);
        }
    })
    .await;
}

// 新增监控配置：请求体与monitor_list.json中单个监控配置对象结构一致，方便页面配置
pub async fn create_monitor(
    monitor_service: web::Data<MonitorService>,
    scheduler: web::Data<Arc<Mutex<Scheduler>>>, // 配置变更后重建监控任务（热更新）
    default_interval: web::Data<u64>,            // interval未配置时的缺省间隔（与调度器同源）
    body: web::Json<MonitorDefinition>,
) -> Result<HttpResponse, actix_web::Error> {
    let entry = body.into_inner();
    super::validation::validate_config(&entry)?;
    let insert: MonitorConfigInsert =
        build_monitor_insert(&entry, Arc::unwrap_or_clone(default_interval.into_inner()));
    // 同步 diesel 写入挪阻塞池（ADR-0002 A1）；此处保留原始 diesel::Error 以识别
    // 唯一约束冲突（映射 409），故不走 run_blocking（它会把错误脱敏吞掉类型）
    let svc = monitor_service.get_ref().clone();
    let insert_for_block = insert.clone();
    let created = web::block(move || svc.create_monitor(&insert_for_block))
        .await
        .map_err(|e| {
            tracing::error!("创建监控阻塞任务失败: {}", e);
            actix_web::error::ErrorInternalServerError("内部错误")
        })?;
    let created = match created {
        Ok(c) => c,
        Err(e) => {
            // 仅唯一约束冲突（name重复）返回409便于前端提示，错误体保持统一JSON形状；
            // 其余（锁超时/磁盘满/连接池耗尽等）是服务端故障，归500避免误导调用方重试
            if matches!(
                &e,
                diesel::result::Error::DatabaseError(
                    diesel::result::DatabaseErrorKind::UniqueViolation,
                    _,
                )
            ) {
                return Ok(HttpResponse::Conflict().json(DefaultResponseObj {
                    code: 409,
                    message: format!(
                        "监控名称已存在: {}（按target/类型生成，请先删除同名配置）",
                        crate::domain::display_name(&entry)
                    ),
                    data: serde_json::Value::Null,
                }));
            }
            return Err(internal_error(e));
        }
    };
    // 热更新：整体重建监控任务，让新配置立即生效（无需重启进程）
    reload_scheduler(&scheduler, &monitor_service).await;
    Ok(HttpResponse::Ok().json(DefaultResponseObj {
        code: 200,
        message: "created".to_string(),
        data: created,
    }))
}

// 更新监控配置：请求体同新增，enabled状态保持不变
pub async fn update_monitor(
    monitor_service: web::Data<MonitorService>,
    scheduler: web::Data<Arc<Mutex<Scheduler>>>,
    default_interval: web::Data<u64>, // interval未配置时的缺省间隔（与调度器同源）
    path: web::Path<i32>,
    body: web::Json<MonitorDefinition>,
) -> Result<HttpResponse, actix_web::Error> {
    let id = path.into_inner();
    let entry = body.into_inner();
    super::validation::validate_config(&entry)?;
    let target = entry
        .target
        .clone()
        .unwrap_or_else(|| entry.monitor_type.to_string());
    let method = entry.method.as_ref().map(|m| {
        serde_json::to_string(m)
            .unwrap_or_default()
            .trim_matches('"')
            .to_string()
    });
    let update = MonitorConfigUpdate {
        name: Some(target.clone()),
        target: Some(target),
        method,
        monitor_type: Some(entry.monitor_type.to_string()),
        interval_ms: Some(
            (entry
                .interval
                .unwrap_or(Arc::unwrap_or_clone(default_interval.into_inner()))
                * 1000) as i32,
        ),
        timeout_ms: Some(entry.timeout.unwrap_or(5000) as i32),
        config_json: serde_json::to_string(&entry).ok(),
        enabled: None, // 更新时保持启用状态不变
        // 分组标签随更新一并写入（与create一致，空串归一为None）
        tag: crate::database::services::normalize_tag(entry.tag.as_deref()),
    };
    // 同步 diesel 写入挪阻塞池（ADR-0002 A1）；保留原始 diesel::Error 以识别
    // 唯一约束冲突（映射 409），故不走 run_blocking
    let svc = monitor_service.get_ref().clone();
    let update_for_block = update.clone();
    let affected = web::block(move || svc.update_monitor(id, &update_for_block))
        .await
        .map_err(|e| {
            tracing::error!("更新监控阻塞任务失败: {}", e);
            actix_web::error::ErrorInternalServerError("内部错误")
        })?;
    let affected = match affected {
        Ok(n) => n,
        Err(e) => {
            // 与create镜像：仅唯一约束冲突（name重复）返回409便于前端提示，
            // 其余服务端故障归500避免误导调用方重试
            if matches!(
                &e,
                diesel::result::Error::DatabaseError(
                    diesel::result::DatabaseErrorKind::UniqueViolation,
                    _,
                )
            ) {
                return Ok(HttpResponse::Conflict().json(DefaultResponseObj {
                    code: 409,
                    message: format!(
                        "监控名称已存在: {}（按target/类型生成，请先删除同名配置）",
                        crate::domain::display_name(&entry)
                    ),
                    data: serde_json::Value::Null,
                }));
            }
            return Err(internal_error(e));
        }
    };
    if affected == 0 {
        return Err(actix_web::error::ErrorNotFound(format!(
            "Monitor {} not found",
            id
        )));
    }
    // 热更新：重建监控任务，让新配置立即生效
    reload_scheduler(&scheduler, &monitor_service).await;
    Ok(HttpResponse::Ok().json(DefaultResponseObj {
        code: 200,
        message: "updated".to_string(),
        data: affected,
    }))
}

// 删除监控配置
pub async fn delete_monitor(
    monitor_service: web::Data<MonitorService>,
    scheduler: web::Data<Arc<Mutex<Scheduler>>>,
    metrics: web::Data<Arc<MetricsRegistry>>,
    path: web::Path<i32>,
) -> Result<HttpResponse, actix_web::Error> {
    let id = path.into_inner();
    // 同步 diesel 写入挪阻塞池（ADR-0002 A1），错误脱敏为通用 500（E1）
    let svc = monitor_service.get_ref().clone();
    let affected = run_blocking(move || svc.delete_monitor(id)).await?;
    if affected == 0 {
        return Err(actix_web::error::ErrorNotFound(format!(
            "Monitor {} not found",
            id
        )));
    }
    // 清理该监控的内存指标：删除是id永久消失的路径，否则/metrics会持续输出陈旧entry
    // MetricsRegistry 内部自带带中毒恢复的 Mutex（ADR-0007 D1：去掉冗余的外层 Arc<Mutex>）
    metrics.remove(id);
    // 热更新：重建监控任务，被删除的监控随之停止
    reload_scheduler(&scheduler, &monitor_service).await;
    Ok(HttpResponse::Ok().json(DefaultResponseObj {
        code: 200,
        message: "deleted".to_string(),
        data: affected,
    }))
}

// 启用/禁用请求体
#[derive(Debug, serde::Deserialize)]
pub struct EnabledBody {
    pub enabled: bool,
}

// 启用/禁用监控配置：禁用的任务被停止，启用的任务被拉起
pub async fn update_monitor_enabled(
    monitor_service: web::Data<MonitorService>,
    scheduler: web::Data<Arc<Mutex<Scheduler>>>,
    path: web::Path<i32>,
    body: web::Json<EnabledBody>,
) -> Result<HttpResponse, actix_web::Error> {
    let id = path.into_inner();
    // 同步 diesel 写入挪阻塞池（ADR-0002 A1），错误脱敏为通用 500（E1）
    let svc = monitor_service.get_ref().clone();
    let enabled = body.enabled;
    let affected = run_blocking(move || svc.set_enabled(id, enabled)).await?;
    if affected == 0 {
        return Err(actix_web::error::ErrorNotFound(format!(
            "Monitor {} not found",
            id
        )));
    }
    reload_scheduler(&scheduler, &monitor_service).await;
    Ok(HttpResponse::Ok().json(DefaultResponseObj {
        code: 200,
        message: "updated".to_string(),
        data: body.enabled,
    }))
}

// POST /api/monitors/{id}/run：立即手动执行一次检查并返回结果
// 结果照常走指标更新与数据库持久化，与定时任务同一条链路
pub async fn run_monitor_once(
    path: web::Path<i32>,
    monitor_service: web::Data<MonitorService>,
    result_service: web::Data<ResultService>,
    metrics: web::Data<Arc<MetricsRegistry>>,
    default_interval: web::Data<u64>,
) -> Result<HttpResponse, actix_web::Error> {
    let id = path.into_inner();
    // 同步 diesel 查询挪阻塞池（ADR-0002 A1），错误脱敏为通用 500（E1）
    let svc = monitor_service.get_ref().clone();
    let row = run_blocking(move || svc.get_monitor_by_id(id))
        .await?
        .ok_or_else(|| actix_web::error::ErrorNotFound(format!("监控 {} 不存在", id)))?;
    // 单次执行的超时上限：取配置 timeout_ms + 10s 余量（覆盖建连/收尾），
    // 兜底 30s；避免个别卡死的探测把请求无限挂起
    let run_timeout_ms = (row.timeout_ms.max(0) as u64).saturating_add(10_000);
    let config_json = row
        .config_json
        .clone()
        .ok_or_else(|| actix_web::error::ErrorBadRequest("监控缺少config_json，无法执行"))?;
    let entry: MonitorDefinition = serde_json::from_str(&config_json)
        .map_err(|e| actix_web::error::ErrorInternalServerError(format!("配置解析失败: {}", e)))?;
    let name = row
        .name
        .clone()
        .unwrap_or_else(|| crate::domain::display_name(&entry));
    // 未配置interval时使用--interval默认值（与定时调度同源）
    let interval_default = Arc::unwrap_or_clone(default_interval.into_inner());
    let config = crate::domain::MonitorConfig::from_entry(&entry, interval_default);
    let monitor = MonitorFactory::create_monitor(config.monitor_type);
    // 单次执行：复用Once模式的执行封装，结果完整返回给前端
    let mut rx = AsyncMonitor::create_once_monitoring(monitor, config).await;
    // ADR-0004 C2：用 timeout 包裹 recv，防止卡死探测把 HTTP 请求无限挂起；
    // 超时返回 504（Gateway Timeout），语义上"上游探测未在时限内返回"
    let result = match tokio::time::timeout(
        std::time::Duration::from_millis(run_timeout_ms),
        rx.recv(),
    )
    .await
    {
        Ok(Some(result)) => result,
        Ok(None) => {
            return Err(actix_web::error::ErrorInternalServerError(
                "监控执行未返回结果",
            ));
        }
        Err(_) => {
            tracing::error!("手动执行监控超时 (monitor_id={}, timeout={}ms)", id, run_timeout_ms);
            return Err(actix_web::error::ErrorGatewayTimeout("监控执行超时"));
        }
    };
    // 指标与持久化与主消费循环同源，手动执行不影响数据一致性
    // MetricsRegistry 内部自带带中毒恢复的 Mutex（ADR-0007 D1：去掉冗余的外层 Arc<Mutex>）
    {
        let (status, response_time, _code) = result.log_fields();
        metrics.record(
            &ResultRoute {
                name: name.clone(),
                monitor_id: Some(id),
            },
            &result.monitor_type.to_string(),
            status,
            u64::try_from(response_time).unwrap_or(u64::MAX),
        );
    }
    // 响应字段先于落库取出：持久化会把 result move 进阻塞闭包
    let (status, response_time, status_code) = result.log_fields();
    let detail_json = serde_json::to_value(&result.details).ok();
    let check_id_hex = format!("{:x}", result.id);
    // 手动执行即时单条落库（不走攒批，结果需立即可查）；同步 diesel 挪阻塞池（A1）
    let result_svc = result_service.get_ref().clone();
    match web::block(move || result_svc.persist_check(id, &result)).await {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => tracing::error!("监控结果持久化失败 (monitor_id={}): {}", id, e),
        Err(e) => tracing::error!("持久化阻塞任务失败 (monitor_id={}): {}", id, e),
    }
    Ok(HttpResponse::Ok().json(DefaultResponseObj {
        code: 200,
        message: "OK".to_string(),
        data: serde_json::json!({
            "check_id": check_id_hex,
            "monitor": name,
            "status": status,
            "response_time": response_time,
            "status_code": status_code,
            "details": detail_json,
        }),
    }))
}

#[cfg(test)]
mod tests {
    use super::{create_monitor, get_all_monitors, get_monitor_tags, update_monitor};
    use crate::api::handles::response::DefaultResponseObj;
    use crate::database::pool::test_pool;
    use crate::database::repositories::monitor_repo::MonitorRepository;
    use crate::database::services::monitor_service::MonitorService;
    use crate::scheduler::Scheduler;
    use actix_web::web;
    use std::sync::{Arc, Mutex};

    // tag 全链路：创建携带 tag → 列表 ?tag= 精确筛选 → /monitors/tags 去重列出
    #[actix_web::test]
    async fn tag_create_filter_and_tags_endpoint() {
        let dir = tempfile::tempdir().unwrap();
        let pool = Arc::new(test_pool(dir.path()));
        let monitor_service = MonitorService::new(MonitorRepository::new(pool.clone()));
        let scheduler = Arc::new(Mutex::new(Scheduler::new(pool.clone(), 5)));

        let app = actix_web::test::init_service(
            actix_web::App::new()
                .app_data(web::Data::new(monitor_service.clone()))
                .app_data(web::Data::new(scheduler.clone()))
                .app_data(web::Data::new(5u64))
                .route("/api/monitors", web::post().to(create_monitor))
                .route("/api/monitors", web::get().to(get_all_monitors))
                .route("/api/monitors/tags", web::get().to(get_monitor_tags)),
        )
        .await;

        let post = |body: &'static str| {
            actix_web::test::TestRequest::post()
                .uri("/api/monitors")
                .insert_header(("Content-Type", "application/json"))
                .set_payload(body.to_string())
                .to_request()
        };
        for body in [
            r#"{"target":"https://a.example.com","monitor_type":"HTTP","tag":"prod"}"#,
            r#"{"target":"https://b.example.com","monitor_type":"HTTP","tag":"prod"}"#,
            r#"{"target":"https://c.example.com","monitor_type":"HTTP","tag":"staging"}"#,
        ] {
            let resp = actix_web::test::call_service(&app, post(body)).await;
            assert_eq!(resp.status(), 200, "创建应成功");
        }

        // ?tag=prod 只回两条
        let resp = actix_web::test::call_service(
            &app,
            actix_web::test::TestRequest::get()
                .uri("/api/monitors?tag=prod")
                .to_request(),
        )
        .await;
        assert_eq!(resp.status(), 200);
        let body: DefaultResponseObj<serde_json::Value> =
            actix_web::test::read_body_json(resp).await;
        assert_eq!(body.data["total"], 2, "prod 应筛出两条");

        // /monitors/tags 去重升序
        let resp = actix_web::test::call_service(
            &app,
            actix_web::test::TestRequest::get()
                .uri("/api/monitors/tags")
                .to_request(),
        )
        .await;
        assert_eq!(resp.status(), 200);
        let body: DefaultResponseObj<Vec<String>> = actix_web::test::read_body_json(resp).await;
        assert_eq!(body.data, vec!["prod".to_string(), "staging".to_string()]);
    }

    // 重复创建：name由target生成且库里有唯一约束，冲突必须映射409而非500
    #[actix_web::test]
    async fn duplicate_name_create_returns_409() {
        let dir = tempfile::tempdir().unwrap();
        let pool = Arc::new(test_pool(dir.path()));
        let monitor_service = MonitorService::new(MonitorRepository::new(pool.clone()));
        let scheduler = Arc::new(Mutex::new(Scheduler::new(pool.clone(), 5)));

        let app = actix_web::test::init_service(
            actix_web::App::new()
                .app_data(web::Data::new(monitor_service.clone()))
                .app_data(web::Data::new(scheduler.clone()))
                .app_data(web::Data::new(5u64)) // create_monitor提取缺省间隔
                .route("/api/monitors", web::post().to(create_monitor)),
        )
        .await;

        let body = r#"{"target":"https://dup.example.com","monitor_type":"HTTP","interval":10}"#;
        let send = |body: &'static str| {
            actix_web::test::TestRequest::post()
                .uri("/api/monitors")
                .insert_header(("Content-Type", "application/json"))
                .set_payload(body.to_string())
                .to_request()
        };
        let resp = actix_web::test::call_service(&app, send(body)).await;
        assert_eq!(resp.status(), 200, "首次创建应成功");

        // 同target重复创建：唯一约束冲突 → 409
        let resp = actix_web::test::call_service(&app, send(body)).await;
        assert_eq!(resp.status(), 409, "重复创建应返回409");
        let err: DefaultResponseObj<serde_json::Value> =
            actix_web::test::read_body_json(resp).await;
        assert!(err.message.contains("已存在"), "错误消息应指明名称冲突");
    }

    // 缺省间隔落库一致性 + 更新撞名409：
    // create/update两条DB写路径的interval缺省值与调度器from_entry同源（app_data注入）；
    // update撞名镜像create的409
    #[actix_web::test]
    async fn update_paths_use_default_interval_and_409() {
        let dir = tempfile::tempdir().unwrap();
        let pool = Arc::new(test_pool(dir.path()));
        let monitor_service = MonitorService::new(MonitorRepository::new(pool.clone()));
        let scheduler = Arc::new(Mutex::new(Scheduler::new(pool.clone(), 5)));

        let app = actix_web::test::init_service(
            actix_web::App::new()
                .app_data(web::Data::new(monitor_service.clone()))
                .app_data(web::Data::new(scheduler.clone()))
                .app_data(web::Data::new(7u64)) // 服务端缺省间隔=7秒
                .route("/api/monitors", web::post().to(create_monitor))
                .route("/api/monitors/{id}", web::put().to(update_monitor)),
        )
        .await;

        // 创建时不带interval：DB列应写服务端缺省7秒
        let body = r#"{"target":"https://a.example.com","monitor_type":"HTTP"}"#;
        let send_post = |uri: &'static str, body: &'static str| {
            actix_web::test::TestRequest::post()
                .uri(uri)
                .insert_header(("Content-Type", "application/json"))
                .set_payload(body.to_string())
                .to_request()
        };
        let resp = actix_web::test::call_service(&app, send_post("/api/monitors", body)).await;
        assert_eq!(resp.status(), 200, "创建应成功");
        let row = monitor_service
            .find_by_name("https://a.example.com")
            .expect("查询应成功")
            .expect("应存在");
        assert_eq!(
            row.interval_ms,
            Some(7000),
            "缺省interval应取app_data注入的7秒"
        );

        // 更新同样不带interval：DB列应保持服务端缺省，而非被重置成硬编码5
        let resp = actix_web::test::call_service(
            &app,
            actix_web::test::TestRequest::put()
                .uri(&format!("/api/monitors/{}", row.id))
                .insert_header(("Content-Type", "application/json"))
                .set_payload(body.to_string())
                .to_request(),
        )
        .await;
        assert_eq!(resp.status(), 200, "更新应成功");
        let row = monitor_service
            .find_by_name("https://a.example.com")
            .expect("查询应成功")
            .expect("应存在");
        assert_eq!(row.interval_ms, Some(7000), "更新后缺省interval不应被重置");

        // 更新撞名：把另一个监控的target改成与a重复 → UniqueViolation → 409
        let _ = actix_web::test::call_service(
            &app,
            send_post(
                "/api/monitors",
                r#"{"target":"https://b.example.com","monitor_type":"HTTP"}"#,
            ),
        )
        .await;
        let resp = actix_web::test::call_service(
            &app,
            actix_web::test::TestRequest::put()
                .uri(&format!("/api/monitors/{}", row.id))
                .insert_header(("Content-Type", "application/json"))
                .set_payload(
                    r#"{"target":"https://b.example.com","monitor_type":"HTTP"}"#.to_string(),
                )
                .to_request(),
        )
        .await;
        assert_eq!(resp.status(), 409, "更新撞名应返回409");
        let err: DefaultResponseObj<serde_json::Value> =
            actix_web::test::read_body_json(resp).await;
        assert!(err.message.contains("已存在"), "错误消息应指明名称冲突");
    }
}
