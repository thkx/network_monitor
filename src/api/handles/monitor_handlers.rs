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
    DefaultResponseObj, PageData, PaginationParams, clamp_pagination, internal_error,
};

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
        .filter(|s| !s.is_empty());
    let (monitors, total) = monitor_service
        .list_monitors(query.enabled, tag, no, size)
        .map_err(|e| {
            actix_web::error::ErrorInternalServerError(format!("Failed to get monitors: {}", e))
        })?;
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
    let tags = monitor_service.get_distinct_tags().map_err(internal_error)?;
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
    let monitor = monitor_service
        .get_monitor_by_id(id)
        .map_err(internal_error)?;
    match monitor {
        Some(m) => Ok(HttpResponse::Ok().json(DefaultResponseObj {
            code: 200,
            message: "OK".to_string(),
            data: m,
        })),
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
    let created = match monitor_service.create_monitor(&insert) {
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
    let affected = match monitor_service.update_monitor(id, &update) {
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
    metrics: web::Data<Arc<Mutex<MetricsRegistry>>>,
    path: web::Path<i32>,
) -> Result<HttpResponse, actix_web::Error> {
    let id = path.into_inner();
    let affected = monitor_service.delete_monitor(id).map_err(internal_error)?;
    if affected == 0 {
        return Err(actix_web::error::ErrorNotFound(format!(
            "Monitor {} not found",
            id
        )));
    }
    // 清理该监控的内存指标：删除是id永久消失的路径，否则/metrics会持续输出陈旧entry
    metrics.lock().expect("metrics锁中毒").remove(id);
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
    let affected = monitor_service
        .set_enabled(id, body.enabled)
        .map_err(internal_error)?;
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
    metrics: web::Data<Arc<Mutex<MetricsRegistry>>>,
    default_interval: web::Data<u64>,
) -> Result<HttpResponse, actix_web::Error> {
    let id = path.into_inner();
    let row = monitor_service
        .get_monitor_by_id(id)
        .map_err(internal_error)?
        .ok_or_else(|| actix_web::error::ErrorNotFound(format!("监控 {} 不存在", id)))?;
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
    let Some(result) = rx.recv().await else {
        return Err(actix_web::error::ErrorInternalServerError(
            "监控执行未返回结果",
        ));
    };
    // 指标与持久化与主消费循环同源，手动执行不影响数据一致性
    {
        let (status, response_time, _code) = result.log_fields();
        metrics.lock().expect("metrics锁中毒").record(
            &ResultRoute {
                name: name.clone(),
                monitor_id: Some(id),
            },
            &result.monitor_type.to_string(),
            status,
            u64::try_from(response_time).unwrap_or(u64::MAX),
        );
    }
    // 手动执行即时单条落库（不走攒批，结果需立即可查）
    if let Err(e) = result_service.persist_check(id, &result) {
        tracing::error!("监控结果持久化失败 (monitor_id={}): {}", id, e);
    }
    let (status, response_time, status_code) = result.log_fields();
    let detail_json = serde_json::to_value(&result.details).ok();
    Ok(HttpResponse::Ok().json(DefaultResponseObj {
        code: 200,
        message: "OK".to_string(),
        data: serde_json::json!({
            "check_id": format!("{:x}", result.id),
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
