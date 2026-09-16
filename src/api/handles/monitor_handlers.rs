use actix_web::{web, HttpResponse};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

use crate::database::models::{MonitorConfigInsert, MonitorConfigUpdate};
use crate::database::services::monitor_service::MonitorService;
use crate::database::services::{build_monitor_insert};
use crate::scheduler::Scheduler;
use crate::tools_types::{ContentVerificationRules, SelfDefineMonitorConfig};

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

// 获取全部的监控配置处理函数
pub async fn get_all_monitors(
    monitor_service: web::Data<MonitorService>, // 设置统一的业务层的对象，进行相关的数据操作
    query: web::Query<PaginationParams>,        // 分页参数对象
) -> Result<HttpResponse, actix_web::Error> {
    let no = query.page_no.unwrap_or(1);
    let size = query.page_size.unwrap_or(20);
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

// 新增监控配置：请求体与monitor_list.json中单个监控配置对象结构一致，方便页面配置
pub async fn create_monitor(
    monitor_service: web::Data<MonitorService>,
    scheduler: web::Data<Arc<Mutex<Scheduler>>>, // 配置变更后重建监控任务（热更新）
    body: web::Json<SelfDefineMonitorConfig>,
) -> Result<HttpResponse, actix_web::Error> {
    let entry = body.into_inner();
    validate_config(&entry)?;
    let insert: MonitorConfigInsert = build_monitor_insert(&entry);
    let created = monitor_service.create_monitor(&insert).map_err(|e| {
        // name唯一约束冲突等情况返回409，便于前端提示
        actix_web::error::ErrorConflict(format!("Failed to create monitor: {}", e))
    })?;
    // 热更新：整体重建监控任务，让新配置立即生效（无需重启进程）
    scheduler.lock().unwrap().reload_all(&monitor_service);
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
        interval_ms: Some((entry.interval.unwrap_or(5) * 1000) as i32),
        timeout_ms: Some(entry.timeout.unwrap_or(5000) as i32),
        config_json: serde_json::to_string(&entry).ok(),
        enabled: None, // 更新时保持启用状态不变
        tag: None,
    };
    let affected = monitor_service
        .update_monitor(id, &update)
        .map_err(internal_error)?;
    if affected == 0 {
        return Err(actix_web::error::ErrorNotFound(format!(
            "Monitor {} not found",
            id
        )));
    }
    // 热更新：重建监控任务，让新配置立即生效
    scheduler.lock().unwrap().reload_all(&monitor_service);
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
    scheduler.lock().unwrap().reload_all(&monitor_service);
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
    scheduler.lock().unwrap().reload_all(&monitor_service);
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
    Ok(())
}

// 统一500错误转换
fn internal_error(e: diesel::result::Error) -> actix_web::Error {
    actix_web::error::ErrorInternalServerError(format!("Database error: {}", e))
}
