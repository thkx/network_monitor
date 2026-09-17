use actix_web::{web, HttpResponse};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::async_monitor::{AsyncMonitor, ResultRoute};
use crate::database::connect_db::SqlitePool;
use crate::database::models::{CheckResultModel, MonitorConfigInsert, MonitorConfigUpdate};
use crate::database::repositories::alert_state_repo::AlertStateRepository;
use crate::database::services::monitor_service::MonitorService;
use crate::database::services::result_service::ResultService;
use crate::database::services::{build_monitor_insert};
use crate::metrics::MetricsRegistry;
use crate::monitor::MonitorFactory;
use crate::scheduler::Scheduler;
use crate::tools_types::{
    AlertRuleTypes, ContentVerificationRules, SelfDefineMonitorConfig,
};

// 定义统一的返回数据结构
#[derive(Debug, Serialize, Deserialize)]
pub struct DefaultResponseObj<T> {
    pub code: usize,
    pub message: String,
    pub data: T,
}

#[derive(Debug, Deserialize)]
pub struct PaginationParams {
    pub page_no: Option<i64>,
    pub page_size: Option<i64>,
    pub enabled: Option<bool>, // 可选：true/false筛选启停状态，缺省查询全部
}

// 分页返回数据
#[derive(Debug, Serialize)]
pub struct PageData<T: Serialize> {
    pub list: Vec<T>,
    pub total: i64,
    pub page_no: i64,
    pub page_size: i64,
}

// 分页参数防御：page_no≥1，page_size限制在1..=500
// （SQLite里LIMIT为负等价于无限制，page_size=-1即全表导出；超大值会整表拉进内存）
pub(crate) fn clamp_pagination(page_no: Option<i64>, page_size: Option<i64>) -> (i64, i64) {
    let no = page_no.unwrap_or(1).max(1);
    let size = page_size.unwrap_or(20).clamp(1, 500);
    (no, size)
}

// 获取全部的监控配置处理函数
pub async fn get_all_monitors(
    monitor_service: web::Data<MonitorService>, // 设置统一的业务层的对象，进行相关的数据操作
    query: web::Query<PaginationParams>,        // 分页参数对象
) -> Result<HttpResponse, actix_web::Error> {
    let (no, size) = clamp_pagination(query.page_no, query.page_size);
    // enabled参数可选：此前硬编码只查启用项，禁用的配置在列表里"消失"
    let (monitors, total) = match query.enabled {
        Some(flag) => monitor_service.get_monitors_by_enabled(flag, no, size),
        None => monitor_service.get_monitors_paged(no, size),
    }
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
    body: web::Json<SelfDefineMonitorConfig>,
) -> Result<HttpResponse, actix_web::Error> {
    let entry = body.into_inner();
    validate_config(&entry)?;
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
                        crate::tools_types::display_name(&entry)
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
    body: web::Json<SelfDefineMonitorConfig>,
) -> Result<HttpResponse, actix_web::Error> {
    let id = path.into_inner();
    let entry = body.into_inner();
    validate_config(&entry)?;
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
            (entry.interval.unwrap_or(Arc::unwrap_or_clone(default_interval.into_inner())) * 1000)
                as i32,
        ),
        timeout_ms: Some(entry.timeout.unwrap_or(5000) as i32),
        config_json: serde_json::to_string(&entry).ok(),
        enabled: None, // 更新时保持启用状态不变
        tag: None,
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
                        crate::tools_types::display_name(&entry)
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
    path: web::Path<i32>,
) -> Result<HttpResponse, actix_web::Error> {
    let id = path.into_inner();
    let affected = monitor_service
        .delete_monitor(id)
        .map_err(internal_error)?;
    if affected == 0 {
        return Err(actix_web::error::ErrorNotFound(format!(
            "Monitor {} not found",
            id
        )));
    }
    // 热更新：重建监控任务，被删除的监控随之停止
    reload_scheduler(&scheduler, &monitor_service).await;
    Ok(HttpResponse::Ok().json(DefaultResponseObj {
        code: 200,
        message: "deleted".to_string(),
        data: affected,
    }))
}

// 启用/禁用请求体
#[derive(Debug, Deserialize)]
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

// 创建/更新前的配置校验：非法配置返回400，避免脏配置入库
fn validate_config(entry: &SelfDefineMonitorConfig) -> Result<(), actix_web::Error> {
    // interval下限1秒：0会造成高频空转甚至busy-loop轰炸目标；上限1天防误配
    if let Some(v) = entry.interval {
        if v == 0 {
            return Err(actix_web::error::ErrorBadRequest(
                "interval 至少为 1 秒（0 会造成高频空转轰炸目标）",
            ));
        }
        if v > 86_400 {
            return Err(actix_web::error::ErrorBadRequest(
                "interval 不能超过 86400 秒（1天）",
            ));
        }
    }
    // timeout合法范围 1ms ~ 5分钟
    if let Some(v) = entry.timeout
        && (v == 0 || v > 300_000)
    {
        return Err(actix_web::error::ErrorBadRequest(
            "timeout 需在 1 ~ 300000 毫秒之间",
        ));
    }
    // 内容规则里的正则：非法正则拒绝入库（引擎侧另有兜底不panic）
    for rule in entry.content_evaluation_rules.iter().flatten() {
        if matches!(rule.rule_type, ContentVerificationRules::Regex)
            && let Err(e) = Regex::new(&rule.rule_content)
        {
            return Err(actix_web::error::ErrorBadRequest(format!(
                "非法正则表达式 {:?}: {}",
                rule.rule_content, e
            )));
        }
    }
    // 告警配置校验：防抖参数范围 + THRESHOLD规则的阈值条件
    if let Some(cfg) = entry.alert_rules.as_ref() {
        for (label, n) in [
            ("consecutive_failures", cfg.consecutive_failures),
            ("consecutive_successes", cfg.consecutive_successes),
        ] {
            if let Some(v) = n
                && (v == 0 || v > 1000)
            {
                return Err(actix_web::error::ErrorBadRequest(format!(
                    "{} 需在 1 ~ 1000 之间",
                    label
                )));
            }
        }
        for rule in cfg.rules.iter() {
            if rule.rule_type == AlertRuleTypes::Threshold {
                let Some(th) = rule.condition.threshold.as_ref() else {
                    return Err(actix_web::error::ErrorBadRequest(
                        "THRESHOLD 规则必须配置 threshold 条件",
                    ));
                };
                if !matches!(th.op.as_str(), ">" | ">=" | "<" | "<=" | "==" | "=") {
                    return Err(actix_web::error::ErrorBadRequest(format!(
                        "THRESHOLD 规则的 op 非法: {:?}（支持 > >= < <= ==）",
                        th.op
                    )));
                }
            }
        }
    }
    Ok(())
}

// 控制台聚合视图：监控配置 + 各自最新一次检查结果 + 告警抑制状态
// 一次请求拿齐列表页所需全部数据，前端无需逐个监控再查结果
#[derive(Debug, Serialize, Deserialize)]
pub struct MonitorStatusItem {
    pub id: i32,
    pub name: String,
    pub target: String,
    pub monitor_type: String,
    pub enabled: i32,
    pub last_status: Option<i32>,      // None=尚未执行过检查
    pub last_response_time: Option<i32>,
    pub last_check_at: Option<String>,
    pub alerting: bool,                // 是否处于"已告警未恢复"状态
}

// GET /api/status
pub async fn get_console_status(
    monitor_service: web::Data<MonitorService>,
    result_service: web::Data<ResultService>,
    pool: web::Data<Arc<SqlitePool>>,
) -> Result<HttpResponse, actix_web::Error> {
    let (monitors, _total) = monitor_service
        .get_monitors_paged(1, 1000)
        .map_err(internal_error)?;
    let latest: Vec<CheckResultModel> = result_service
        .get_latest_by_monitor()
        .map_err(internal_error)?;
    let latest_map: HashMap<i32, &CheckResultModel> =
        latest.iter().map(|r| (r.monitor_id, r)).collect();
    // 查库失败降级为空快照：状态页仍可展示，只是alerting全为false
    let alert_map: HashMap<i32, bool> = AlertStateRepository::new(pool.get_ref().clone())
        .get_all_alerting()
        .unwrap_or_default()
        .into_iter()
        .collect();
    let items: Vec<MonitorStatusItem> = monitors
        .iter()
        .map(|m| {
            let l = latest_map.get(&m.id);
            MonitorStatusItem {
                id: m.id,
                name: m.name.clone().unwrap_or_else(|| m.target.clone()),
                target: m.target.clone(),
                monitor_type: m.monitor_type.clone(),
                enabled: m.enabled,
                last_status: l.map(|r| r.status),
                last_response_time: l.map(|r| r.response_time),
                last_check_at: l.and_then(|r| {
                    r.created_at
                        .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
                }),
                alerting: alert_map.get(&m.id).copied().unwrap_or(false),
            }
        })
        .collect();
    Ok(HttpResponse::Ok().json(DefaultResponseObj {
        code: 200,
        message: "OK".to_string(),
        data: items,
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
    let entry: SelfDefineMonitorConfig = serde_json::from_str(&config_json)
        .map_err(|e| {
            actix_web::error::ErrorInternalServerError(format!("配置解析失败: {}", e))
        })?;
    let name = row
        .name
        .clone()
        .unwrap_or_else(|| crate::tools_types::display_name(&entry));
    // 未配置interval时使用--interval默认值（与定时调度同源）
    let interval_default = Arc::unwrap_or_clone(default_interval.into_inner());
    let config = crate::monitor::types::MonitorConfig::from_entry(&entry, interval_default);
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

// 统一500错误转换
fn internal_error(e: diesel::result::Error) -> actix_web::Error {
    actix_web::error::ErrorInternalServerError(format!("Database error: {}", e))
}

#[cfg(test)]
mod tests {
    use super::validate_config;
    use crate::tools_types::SelfDefineMonitorConfig;

    #[test]
    fn pagination_is_clamped() {
        use super::clamp_pagination;
        // 缺省值
        assert_eq!(clamp_pagination(None, None), (1, 20));
        // 负数size在SQLite里等价LIMIT无限制（全表导出），必须夹到1
        assert_eq!(clamp_pagination(Some(0), Some(-1)), (1, 1));
        // 超大size夹到500上限
        assert_eq!(clamp_pagination(Some(3), Some(100000)), (3, 500));
        // 正常值原样通过
        assert_eq!(clamp_pagination(Some(2), Some(50)), (2, 50));
    }

    fn entry_from_json(json: &str) -> SelfDefineMonitorConfig {
        serde_json::from_str(json).expect("测试JSON应可反序列化")
    }

    #[test]
    fn zero_interval_is_rejected() {
        let entry =
            entry_from_json(r#"{"target":"https://a.com","monitor_type":"HTTP","interval":0}"#);
        assert!(validate_config(&entry).is_err());
    }

    #[test]
    fn interval_over_one_day_is_rejected() {
        let entry =
            entry_from_json(r#"{"target":"https://a.com","monitor_type":"HTTP","interval":86401}"#);
        assert!(validate_config(&entry).is_err());
    }

    #[test]
    fn boundary_interval_is_accepted() {
        let entry =
            entry_from_json(r#"{"target":"https://a.com","monitor_type":"HTTP","interval":86400}"#);
        assert!(validate_config(&entry).is_ok());
    }

    #[test]
    fn invalid_timeout_is_rejected() {
        let zero =
            entry_from_json(r#"{"target":"https://a.com","monitor_type":"HTTP","timeout":0}"#);
        assert!(validate_config(&zero).is_err());
        let over =
            entry_from_json(r#"{"target":"https://a.com","monitor_type":"HTTP","timeout":300001}"#);
        assert!(validate_config(&over).is_err());
    }

    #[test]
    fn invalid_regex_is_rejected() {
        let entry = entry_from_json(
            r#"{"target":"https://a.com","monitor_type":"HTTP","content_evaluation_rules":[{"rule_type":"regex","rule_content":"([bad","rule_description":""}]}"#,
        );
        assert!(validate_config(&entry).is_err());
    }

    #[test]
    fn valid_config_passes() {
        let entry = entry_from_json(
            r#"{"target":"https://a.com","monitor_type":"HTTP","interval":10,"timeout":5000,"content_evaluation_rules":[{"rule_type":"regex","rule_content":"5\\d\\d","rule_description":""}]}"#,
        );
        assert!(validate_config(&entry).is_ok());
    }

    #[test]
    fn debounce_zero_is_rejected() {
        let entry = entry_from_json(
            r#"{"target":"https://a.com","monitor_type":"HTTP","alert_rules":{"notify_type":"FEISHU","notify_config":{"webhook_url":"http://x"},"rules":[],"consecutive_failures":0}}"#,
        );
        assert!(validate_config(&entry).is_err());
    }

    #[test]
    fn debounce_upper_bound_is_rejected() {
        let entry = entry_from_json(
            r#"{"target":"https://a.com","monitor_type":"HTTP","alert_rules":{"notify_type":"FEISHU","notify_config":{"webhook_url":"http://x"},"rules":[],"consecutive_successes":1001}}"#,
        );
        assert!(validate_config(&entry).is_err());
    }

    #[test]
    fn debounce_valid_value_passes() {
        let entry = entry_from_json(
            r#"{"target":"https://a.com","monitor_type":"HTTP","alert_rules":{"notify_type":"FEISHU","notify_config":{"webhook_url":"http://x"},"rules":[],"consecutive_failures":3,"consecutive_successes":2}}"#,
        );
        assert!(validate_config(&entry).is_ok());
    }

    #[test]
    fn threshold_rule_requires_condition() {
        // THRESHOLD规则缺threshold条件：拒绝
        let missing = entry_from_json(
            r#"{"target":"x","monitor_type":"CPU","alert_rules":{"notify_type":"FEISHU","notify_config":{"webhook_url":"http://x"},"rules":[{"rule_type":"THRESHOLD","condition":{}}]}}"#,
        );
        assert!(validate_config(&missing).is_err());
    }

    #[test]
    fn threshold_rule_rejects_bad_op() {
        let bad = entry_from_json(
            r#"{"target":"x","monitor_type":"CPU","alert_rules":{"notify_type":"FEISHU","notify_config":{"webhook_url":"http://x"},"rules":[{"rule_type":"THRESHOLD","condition":{"threshold":{"metric":"cpu","op":"~","value":80}}}]}}"#,
        );
        assert!(validate_config(&bad).is_err());
        // 合法op通过
        let good = entry_from_json(
            r#"{"target":"x","monitor_type":"CPU","alert_rules":{"notify_type":"FEISHU","notify_config":{"webhook_url":"http://x"},"rules":[{"rule_type":"THRESHOLD","condition":{"threshold":{"metric":"cpu","op":">=","value":80}}}]}}"#,
        );
        assert!(validate_config(&good).is_ok());
    }

    // 重复创建：name由target生成且库里有唯一约束，冲突必须映射409而非500
    #[actix_web::test]
    async fn duplicate_name_create_returns_409() {
        use super::{create_monitor, DefaultResponseObj};
        use actix_web::web;
        use crate::database::connect_db::test_pool;
        use crate::database::repositories::monitor_repo::MonitorRepository;
        use crate::database::services::monitor_service::MonitorService;
        use crate::scheduler::Scheduler;
        use std::sync::{Arc, Mutex};

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

        // 同target重复创建：唯一约束冲突 → 409（此前一切创建错误都被误分类为409，
        // 现在只有UniqueViolation返回409，但此用例验证正向路径仍然正确）
        let resp = actix_web::test::call_service(&app, send(body)).await;
        assert_eq!(resp.status(), 409, "重复创建应返回409");
        let err: DefaultResponseObj<serde_json::Value> =
            actix_web::test::read_body_json(resp).await;
        assert!(err.message.contains("已存在"), "错误消息应指明名称冲突");
    }

    // 缺省间隔落库一致性 + 更新撞名409：
    // create/update两条DB写路径的interval缺省值此前硬编码5（A1统一间隔时漏网），
    // 现在与调度器from_entry同源（app_data注入）；update撞名此前映射500，现镜像create的409
    #[actix_web::test]
    async fn update_paths_use_default_interval_and_409() {
        use super::{DefaultResponseObj, create_monitor, update_monitor};
        use actix_web::web;
        use crate::database::connect_db::test_pool;
        use crate::database::repositories::monitor_repo::MonitorRepository;
        use crate::database::services::monitor_service::MonitorService;
        use crate::scheduler::Scheduler;
        use std::sync::{Arc, Mutex};

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

        // 创建时不带interval：DB列应写服务端缺省7秒（此前硬编码5）
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
        let row = monitor_service.find_by_name("https://a.example.com")
            .expect("查询应成功")
            .expect("应存在");
        assert_eq!(row.interval_ms, Some(7000), "缺省interval应取app_data注入的7秒");

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
        let row = monitor_service.find_by_name("https://a.example.com")
            .expect("查询应成功")
            .expect("应存在");
        assert_eq!(row.interval_ms, Some(7000), "更新后缺省interval不应被重置");

        // 更新撞名：把另一个监控的target改成与a重复 → UniqueViolation → 409（此前500）
        let _ = actix_web::test::call_service(
            &app,
            send_post("/api/monitors", r#"{"target":"https://b.example.com","monitor_type":"HTTP"}"#),
        )
        .await;
        let resp = actix_web::test::call_service(
            &app,
            actix_web::test::TestRequest::put()
                .uri(&format!("/api/monitors/{}", row.id))
                .insert_header(("Content-Type", "application/json"))
                .set_payload(r#"{"target":"https://b.example.com","monitor_type":"HTTP"}"#.to_string())
                .to_request(),
        )
        .await;
        assert_eq!(resp.status(), 409, "更新撞名应返回409");
        let err: DefaultResponseObj<serde_json::Value> =
            actix_web::test::read_body_json(resp).await;
        assert!(err.message.contains("已存在"), "错误消息应指明名称冲突");
    }

    // 控制台两个新端点的端到端测试：临时库 + 内存App
    // 手动执行选CPU类型监控：无网络依赖，结果稳定可用
    #[actix_web::test]
    async fn console_status_and_manual_run_roundtrip() {
        use super::{DefaultResponseObj, MonitorStatusItem, get_console_status, run_monitor_once};
        use actix_web::web;
        use crate::database::connect_db::test_pool;
        use crate::database::models::MonitorConfigInsert;
        use crate::database::repositories::monitor_repo::MonitorRepository;
        use crate::database::repositories::result_repo::CheckResultRepository;
        use crate::database::services::monitor_service::MonitorService;
        use crate::database::services::result_service::ResultService;
        use crate::metrics::MetricsRegistry;
        use std::sync::{Arc, Mutex};

        let dir = tempfile::tempdir().unwrap();
        let pool = Arc::new(test_pool(dir.path()));
        let monitor_service = MonitorService::new(MonitorRepository::new(pool.clone()));
        let result_service = ResultService::new(CheckResultRepository::new(pool.clone()));
        let metrics = Arc::new(Mutex::new(MetricsRegistry::new()));

        let insert = MonitorConfigInsert {
            name: Some("t-cpu".to_string()),
            target: "cpu://local".to_string(),
            method: None,
            monitor_type: "CPU".to_string(),
            interval_ms: Some(5),
            timeout_ms: 5000,
            config_json: Some(r#"{"monitor_type":"CPU","timeout":5}"#.to_string()),
            enabled: 1,
            tag: None,
        };
        let created = monitor_service.create_monitor(&insert).unwrap();

        let app = actix_web::test::init_service(
            actix_web::App::new()
                .app_data(web::Data::new(monitor_service.clone()))
                .app_data(web::Data::new(result_service.clone()))
                .app_data(web::Data::new(pool.clone()))
                .app_data(web::Data::new(metrics.clone()))
                .app_data(web::Data::new(5u64))
                .route("/api/status", web::get().to(get_console_status))
                .route(
                    "/api/monitors/{id}/run",
                    web::post().to(run_monitor_once),
                ),
        )
        .await;

        // 手动执行：200 + 可用 + 详情返回
        let req = actix_web::test::TestRequest::post()
            .uri(&format!("/api/monitors/{}/run", created.id))
            .to_request();
        let resp = actix_web::test::call_service(&app, req).await;
        assert_eq!(resp.status(), 200, "手动执行应成功");
        let body: DefaultResponseObj<serde_json::Value> =
            actix_web::test::read_body_json(resp).await;
        assert_eq!(body.data["status"], true);
        assert!(body.data["details"].is_object());

        // status聚合：最新结果已落库可见
        let req = actix_web::test::TestRequest::get()
            .uri("/api/status")
            .to_request();
        let resp = actix_web::test::call_service(&app, req).await;
        assert_eq!(resp.status(), 200);
        let body: DefaultResponseObj<Vec<MonitorStatusItem>> =
            actix_web::test::read_body_json(resp).await;
        assert_eq!(body.data.len(), 1);
        assert_eq!(body.data[0].id, created.id);
        assert_eq!(body.data[0].last_status, Some(1), "最新结果应为可用");
        assert!(body.data[0].last_check_at.is_some());
        assert!(!body.data[0].alerting);

        // 手动执行不存在的监控：404
        let req = actix_web::test::TestRequest::post()
            .uri("/api/monitors/99999/run")
            .to_request();
        let resp = actix_web::test::call_service(&app, req).await;
        assert_eq!(resp.status(), 404);
    }
}
