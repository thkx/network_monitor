// 告警引擎：当监控结果产生时，按告警规则判断是否需要告警（消息文本的构造也在这里）
// 支持的规则类型：
//   RESPONSE_CODE —— HTTP响应码规则（仅HTTP监控生效）
//   AVAILABILITY  —— 通用可用性规则（全部监控类型生效：探测不可用即触发）
//   THRESHOLD     —— 阈值规则（系统资源类：CPU/内存/磁盘/进程数值越限）
// 告警防抖：consecutive_failures 连续N次命中才告警、consecutive_successes 连续M次正常才恢复
// 通知渠道的发送与重试见 super::notify（NotifyEngine）

use super::notify::NotifyEngine;
use crate::database::repositories::alert_state_repo::AlertStateRepository;
use crate::monitor::types::{CheckResult, CheckResultDetail};
use crate::tools_types::{
    AlertRuleTypes, AlertVerificationRules, HttpMonitorResult, NotifyCondition,
};
use regex::Regex;

// 告警引擎：持有通知引擎、告警规则与抑制状态，对外提供统一的check入口
pub struct AlertsEngine {
    notify: NotifyEngine,                // 通知引擎
    alert_rules: AlertVerificationRules, // 告警规则配置
    // 告警抑制状态：是否处于"已告警、未恢复"状态（防止同一故障反复轰炸通知渠道）
    alerting: bool,
    // 防抖计数：连续命中/连续正常的次数（仅内存态，重启后重新计数；
    // 阈值取自配置的 consecutive_failures/consecutive_successes，缺省1）
    hit_streak: u32,
    ok_streak: u32,
    // 抑制状态持久化（Server模式）：重启后从 alert_state 表恢复，
    // 避免"故障还在却重复告警"、"恢复时误报"；Once/Monitor模式为None（仅内存态）
    state: Option<(AlertStateRepository, i32)>,
}

impl AlertsEngine {
    // 无持久化：Once/Monitor模式使用（没有DB主键，抑制状态仅存内存）
    pub fn new(alert_rule: AlertVerificationRules) -> Self {
        AlertsEngine {
            alerting: false,
            hit_streak: 0,
            ok_streak: 0,
            state: None,
            alert_rules: alert_rule.clone(),
            notify: NotifyEngine::new(alert_rule.notify_type, alert_rule.notify_config),
        }
    }

    // 带状态持久化：Server模式使用，创建时从 alert_state 表恢复上次的抑制状态
    pub fn with_state(
        alert_rule: AlertVerificationRules,
        repo: AlertStateRepository,
        monitor_id: i32,
    ) -> Self {
        let mut engine = AlertsEngine::new(alert_rule);
        // 恢复失败不阻塞主流程，退化为内存态（等价旧行为）
        match repo.get_alerting(monitor_id) {
            Ok(alerting) => engine.alerting = alerting,
            Err(e) => tracing::error!("恢复告警状态失败 (monitor_id={}): {}", monitor_id, e),
        }
        engine.state = Some((repo, monitor_id));
        engine
    }

    // 保存抑制状态（有持久化时）；失败只打日志，不影响告警主流程
    fn persist_state(&self, alerting: bool) {
        if let Some((repo, monitor_id)) = &self.state
            && let Err(e) = repo.set_alerting(*monitor_id, alerting)
        {
            tracing::error!("保存告警状态失败 (monitor_id={}): {}", monitor_id, e);
        }
    }

    // 告警规则check事件：状态机式告警——异常时只告警一次，恢复时发送一次恢复通知
    // 注意：失败（不可用、未拿到响应码）的监控结果同样参与检查，避免站点宕机时漏告警
    // 防抖：连续 N 次命中才告警、连续 M 次正常才恢复（缺省均为1，即保持"首次即触发"的旧行为）
    pub async fn check(&mut self, check_result: &CheckResult) -> Result<(), String> {
        let failures_threshold = self.alert_rules.consecutive_failures.unwrap_or(1).max(1);
        let successes_threshold = self.alert_rules.consecutive_successes.unwrap_or(1).max(1);
        match self.evaluate(check_result) {
            // 命中规则：连续命中达到阈值且未处于告警状态时发送，之后抑制重复告警
            Some(message) => {
                self.hit_streak = self.hit_streak.saturating_add(1);
                self.ok_streak = 0;
                if !self.alerting && self.hit_streak >= failures_threshold {
                    self.notify.send_alert_message(message).await;
                    self.alerting = true;
                    self.hit_streak = 0;
                    self.persist_state(true);
                }
            }
            // 未命中任何规则视为正常：连续正常达到阈值时发送一次恢复通知
            None => {
                self.ok_streak = self.ok_streak.saturating_add(1);
                self.hit_streak = 0;
                if self.alerting && self.ok_streak >= successes_threshold {
                    let target = check_result.target.clone().unwrap_or_default();
                    self.notify
                        .send_alert_message(format!(
                            "[监控恢复] target: {} | {} | 异常已恢复（{}）",
                            target,
                            check_result.monitor_type,
                            detail_summary(&check_result.details)
                        ))
                        .await;
                    self.alerting = false;
                    self.ok_streak = 0;
                    self.persist_state(false);
                }
            }
        }
        Ok(())
    }

    // 告警规则评估：命中任一规则返回告警消息，正常返回None
    fn evaluate(&self, check_result: &CheckResult) -> Option<String> {
        if self.alert_rules.rules.is_empty() {
            return None;
        }
        let target = check_result.target.clone().unwrap_or_default();
        for single_rule in self.alert_rules.rules.iter() {
            match single_rule.rule_type {
                AlertRuleTypes::ResponseCode => {
                    if let Some(message) =
                        self.evaluate_response_code(&target, check_result, &single_rule.condition)
                    {
                        return Some(message);
                    }
                }
                AlertRuleTypes::Availability => {
                    // 通用可用性规则：探测不可用即触发（全部监控类型生效）
                    if !is_target_available(check_result) {
                        return Some(format!(
                            "[监控告警] target: {} | {} | 探测不可用（{}）",
                            target,
                            check_result.monitor_type,
                            detail_summary(&check_result.details)
                        ));
                    }
                }
                AlertRuleTypes::Threshold => {
                    // 阈值规则：系统资源类监控的数值越限告警
                    if let Some(message) = evaluate_threshold(check_result, &single_rule.condition)
                    {
                        return Some(message);
                    }
                }
                AlertRuleTypes::Content => {
                    // 内容匹配规则（预留）
                }
            }
        }
        None
    }

    // RESPONSE_CODE规则评估（仅HTTP监控有意义）：命中条件返回告警消息
    fn evaluate_response_code(
        &self,
        target: &str,
        check_result: &CheckResult,
        condition: &NotifyCondition,
    ) -> Option<String> {
        // 结果详情不是HTTP类型时无法评估响应码规则，跳过
        let CheckResultDetail::Http(ref http_result) = check_result.details else {
            return None;
        };
        let NotifyCondition {
            contains,
            no_contains,
            regex,
            ..
        } = condition;
        // 未获取到响应码说明请求失败，直接触发告警
        let Some(code) = http_result.basic_avaliable.res_status_code else {
            return Some(format!(
                "[监控告警] target: {} | 请求失败，未获取到响应码（{}）",
                target,
                failure_reason(http_result)
            ));
        };
        // contains列表非空时：响应码在列表内则触发告警
        if !contains.is_empty() && contains.contains(&code) {
            return Some(format!(
                "[监控告警] target: {} | 响应码{}命中异常列表{:?}",
                target, code, contains
            ));
        }
        // no_contains列表非空时：响应码不在列表内则触发告警
        if !no_contains.is_empty() && !no_contains.contains(&code) {
            return Some(format!(
                "[监控告警] target: {} | 响应码{}不在允许列表{:?}）",
                target, code, no_contains
            ));
        }
        // regex非空时：响应码命中正则则触发告警
        if !regex.is_empty()
            && let Ok(regex_r) = Regex::new(regex)
                && regex_r.is_match(&code.to_string()) {
                    return Some(format!(
                        "[监控告警] target: {} | 响应码{}命中正则{}",
                        target, code, regex
                    ));
                }
        None
    }
}

// 目标是否可用：任务执行失败、或各类型结果中的可达性标志为false 都视为不可用
// 系统资源类监控（CPU/MEMORY/DISK/PROCESS）能执行即视为可用（无可用性语义，仅受任务状态影响）
fn is_target_available(check_result: &CheckResult) -> bool {
    if !check_result.status {
        return false;
    }
    match &check_result.details {
        CheckResultDetail::Http(r) => r.basic_avaliable.is_reachable,
        CheckResultDetail::Icmp(r) => r.is_alive,
        CheckResultDetail::Tcp(r) => r.connected,
        CheckResultDetail::Udp(r) => r.response_received,
        CheckResultDetail::Dns(r) => r.resolved,
        CheckResultDetail::Ftp(r) => r.connected,
        CheckResultDetail::Traceroute(r) => r.success,
        CheckResultDetail::Cpu(_)
        | CheckResultDetail::Memory(_)
        | CheckResultDetail::Disk(_)
        | CheckResultDetail::Process(_) => true,
        CheckResultDetail::Unknown(_) => false,
    }
}

// 阈值规则评估（CPU/内存/磁盘/进程有意义）：数值越限返回告警消息
// 任务执行失败时跳过（此时数值不可信，失败场景已由AVAILABILITY规则覆盖）
fn evaluate_threshold(check_result: &CheckResult, condition: &NotifyCondition) -> Option<String> {
    let th = condition.threshold.as_ref()?;
    if !check_result.status {
        return None;
    }
    let metric = th.metric.to_lowercase();
    let (value, unit): (f64, &str) = match &check_result.details {
        CheckResultDetail::Cpu(r) if metric == "cpu" => (r.usage_percent as f64, "%"),
        CheckResultDetail::Memory(r) if metric == "memory" => (r.usage_percent as f64, "%"),
        CheckResultDetail::Disk(r) if metric == "disk" => {
            // 磁盘按总量换算使用率（总容量为0时跳过，避免除零误报）
            if r.total_bytes == 0 {
                return None;
            }
            let used_percent =
                (r.total_bytes - r.available_bytes) as f64 / r.total_bytes as f64 * 100.0;
            (used_percent, "%")
        }
        CheckResultDetail::Disk(r) if metric == "available_bytes" => {
            (r.available_bytes as f64, "B")
        }
        CheckResultDetail::Process(r) if metric == "process" => (r.process_count as f64, "个"),
        // 结果类型与metric不匹配（如cpu规则配在HTTP监控上）：跳过而不是误判
        _ => return None,
    };
    let hit = match th.op.as_str() {
        ">" => value > th.value,
        ">=" => value >= th.value,
        "<" => value < th.value,
        "<=" => value <= th.value,
        "==" | "=" => (value - th.value).abs() < f64::EPSILON,
        // 非法比较符直接跳过（API侧已校验，防御JSON直写数据库的场景）
        _ => false,
    };
    if hit {
        Some(format!(
            "[监控告警] target: {} | {} | {} {} {}（当前 {:.1}{}）",
            check_result.target.as_deref().unwrap_or("-"),
            check_result.monitor_type,
            th.metric,
            th.op,
            th.value,
            value,
            unit
        ))
    } else {
        None
    }
}

// 结果摘要：附加到告警/恢复消息里，值班看到通知即可直接判断故障性质
fn detail_summary(details: &CheckResultDetail) -> String {
    match details {
        CheckResultDetail::Http(r) => match r.basic_avaliable.res_status_code {
            Some(code) => format!("状态码 {}，耗时 {} ms", code, r.performance_timings.total_time),
            None => format!("请求失败（{}）", failure_reason(r)),
        },
        CheckResultDetail::Icmp(r) => format!("存活 {}，耗时 {} ms", r.is_alive, r.elapsed_ms),
        CheckResultDetail::Tcp(r) => format!("连接 {}，耗时 {} ms", r.connected, r.elapsed_ms),
        CheckResultDetail::Udp(r) => format!("响应 {}，耗时 {} ms", r.response_received, r.elapsed_ms),
        CheckResultDetail::Dns(r) => format!("解析 {}，耗时 {} ms", r.resolved, r.elapsed_ms),
        CheckResultDetail::Ftp(r) => format!("连接 {}，耗时 {} ms", r.connected, r.elapsed_ms),
        CheckResultDetail::Traceroute(r) => format!("成功 {}，{} 跳", r.success, r.hops.len()),
        CheckResultDetail::Cpu(r) => format!("使用率 {:.1}%", r.usage_percent),
        CheckResultDetail::Memory(r) => format!("使用率 {:.1}%", r.usage_percent),
        CheckResultDetail::Disk(r) => format!("剩余可用 {} MB", r.available_bytes / 1024 / 1024),
        CheckResultDetail::Process(r) => format!("进程数 {}", r.process_count),
        CheckResultDetail::Unknown(_) => "未知结果".to_string(),
    }
}

// HTTP失败原因：优先使用 error_kind/error_message（增强前产生的旧记录可能为空）
fn failure_reason(r: &HttpMonitorResult) -> String {
    match (&r.error_kind, &r.error_message) {
        (Some(kind), Some(msg)) => format!("{}: {}", kind, msg),
        (Some(kind), None) => kind.clone(),
        (None, Some(msg)) => msg.clone(),
        (None, None) => "未知原因".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AlertsEngine, detail_summary, failure_reason, is_target_available,
    };
    use crate::monitor::types::{
        CheckResult, CheckResultDetail, CpuMonitorResult, DiskMonitorResult, IcmpMonitorResult,
        MemoryMonitorResult,
    };
    use crate::tools_types::{
        AlertRuleTypes, AlertSingleRule, AlertVerificationRules, BasicAvailability,
        HttpMonitorResult, MonitorType, NotifyCondition, NotifyConfig, ThresholdCondition,
    };

    // 构造HTTP类型的检查结果（其余字段走Default）
    fn http_result(status: bool, reachable: bool, code: Option<u16>) -> CheckResult {
        CheckResult {
            id: 1,
            monitor_type: MonitorType::Http,
            target: Some("https://example.com".to_string()),
            status,
            details: CheckResultDetail::Http(HttpMonitorResult {
                basic_avaliable: BasicAvailability {
                    is_reachable: reachable,
                    res_status_code: code,
                    ..Default::default()
                },
                ..Default::default()
            }),
        }
    }

    fn engine_with_rules_cfg(
        rules: Vec<AlertSingleRule>,
        failures: Option<u32>,
        successes: Option<u32>,
    ) -> AlertsEngine {
        AlertsEngine::new(AlertVerificationRules {
            notify_type: "FEISHU".to_string(),
            notify_config: NotifyConfig {
                webhook_url: "http://127.0.0.1:9".to_string(),
                secret: None,
            },
            rules,
            consecutive_failures: failures,
            consecutive_successes: successes,
        })
    }

    fn engine_with_rules(rules: Vec<AlertSingleRule>) -> AlertsEngine {
        engine_with_rules_cfg(rules, None, None)
    }

    fn availability_rule() -> AlertSingleRule {
        AlertSingleRule {
            rule_type: AlertRuleTypes::Availability,
            condition: NotifyCondition::default(),
        }
    }

    fn response_code_rule(no_contains: Vec<u16>) -> AlertSingleRule {
        AlertSingleRule {
            rule_type: AlertRuleTypes::ResponseCode,
            condition: NotifyCondition {
                no_contains,
                contains: vec![],
                regex: String::new(),
                ..Default::default()
            },
        }
    }

    fn threshold_rule(metric: &str, op: &str, value: f64) -> AlertSingleRule {
        AlertSingleRule {
            rule_type: AlertRuleTypes::Threshold,
            condition: NotifyCondition {
                threshold: Some(ThresholdCondition {
                    metric: metric.to_string(),
                    op: op.to_string(),
                    value,
                }),
                ..Default::default()
            },
        }
    }

    fn cpu_result(usage: f32) -> CheckResult {
        CheckResult {
            id: 5,
            monitor_type: MonitorType::Cpu,
            target: None,
            status: true,
            details: CheckResultDetail::Cpu(CpuMonitorResult {
                usage_percent: usage,
                core_count: 8,
            }),
        }
    }

    #[test]
    fn availability_rule_triggers_on_unreachable() {
        let engine = engine_with_rules(vec![availability_rule()]);
        // 任务成功但目标不可达
        assert!(engine.evaluate(&http_result(true, false, None)).is_some());
    }

    #[test]
    fn availability_rule_silent_when_reachable() {
        let engine = engine_with_rules(vec![availability_rule()]);
        assert!(engine
            .evaluate(&http_result(true, true, Some(200)))
            .is_none());
    }

    #[test]
    fn failed_task_counts_as_unavailable() {
        let engine = engine_with_rules(vec![availability_rule()]);
        assert!(engine.evaluate(&http_result(false, true, Some(200))).is_some());
    }

    #[test]
    fn no_rules_never_triggers() {
        let engine = engine_with_rules(vec![]);
        assert!(engine.evaluate(&http_result(true, false, None)).is_none());
    }

    #[test]
    fn response_code_rule_matches_no_contains() {
        let engine = engine_with_rules(vec![response_code_rule(vec![200, 301])]);
        assert!(engine.evaluate(&http_result(true, true, Some(500))).is_some());
        assert!(engine
            .evaluate(&http_result(true, true, Some(200)))
            .is_none());
    }

    #[test]
    fn request_failure_without_code_triggers_response_code_rule() {
        let engine = engine_with_rules(vec![response_code_rule(vec![200])]);
        // 拿不到响应码（请求失败）同样触发告警
        assert!(engine.evaluate(&http_result(true, false, None)).is_some());
    }

    #[test]
    fn response_code_rule_skipped_for_non_http_details() {
        let engine = engine_with_rules(vec![response_code_rule(vec![200])]);
        // ICMP结果没有响应码，RESPONSE_CODE规则直接跳过
        let result = CheckResult {
            id: 2,
            monitor_type: MonitorType::Icmp,
            target: Some("10.0.0.1".to_string()),
            status: true,
            details: CheckResultDetail::Icmp(IcmpMonitorResult {
                is_alive: true,
                elapsed_ms: 30,
            }),
        };
        assert!(engine.evaluate(&result).is_none());
    }

    #[test]
    fn availability_rule_applies_to_icmp() {
        let engine = engine_with_rules(vec![availability_rule()]);
        let down = CheckResult {
            id: 3,
            monitor_type: MonitorType::Icmp,
            target: Some("10.0.0.1".to_string()),
            status: true,
            details: CheckResultDetail::Icmp(IcmpMonitorResult {
                is_alive: false,
                elapsed_ms: 30,
            }),
        };
        assert!(!is_target_available(&down));
        assert!(engine.evaluate(&down).is_some());
    }

    #[test]
    fn system_monitors_available_when_task_ok() {
        let result = CheckResult {
            id: 4,
            monitor_type: MonitorType::Cpu,
            target: None,
            status: true,
            details: CheckResultDetail::Cpu(Default::default()),
        };
        assert!(is_target_available(&result));
        assert!(engine_with_rules(vec![availability_rule()])
            .evaluate(&result)
            .is_none());
    }

    #[test]
    fn failure_reason_combines_kind_and_message() {
        let mut r = HttpMonitorResult::default();
        assert_eq!(failure_reason(&r), "未知原因");
        r.error_kind = Some("timeout".to_string());
        assert_eq!(failure_reason(&r), "timeout");
        r.error_message = Some("boom".to_string());
        assert_eq!(failure_reason(&r), "timeout: boom");
        r.error_kind = None;
        assert_eq!(failure_reason(&r), "boom");
    }

    #[test]
    fn detail_summary_reports_status_code_and_elapsed() {
        assert_eq!(
            detail_summary(&http_result(true, true, Some(200)).details),
            "状态码 200，耗时 0 ms"
        );
        let failed = http_result(false, false, None);
        assert_eq!(detail_summary(&failed.details), "请求失败（未知原因）");
    }

    #[tokio::test]
    async fn default_threshold_alerts_immediately() {
        // 缺省（未配置防抖）：首次命中即告警，保持旧行为
        let mut engine = engine_with_rules(vec![availability_rule()]);
        engine
            .check(&http_result(true, false, None))
            .await
            .unwrap();
        assert!(engine.alerting);
    }

    #[tokio::test]
    async fn debounce_requires_consecutive_failures() {
        let mut engine = engine_with_rules_cfg(vec![availability_rule()], Some(3), None);
        engine
            .check(&http_result(true, false, None))
            .await
            .unwrap();
        assert!(!engine.alerting, "第1次命中未达阈值");
        engine
            .check(&http_result(true, false, None))
            .await
            .unwrap();
        assert!(!engine.alerting, "第2次命中仍未达阈值");
        engine
            .check(&http_result(true, false, None))
            .await
            .unwrap();
        assert!(engine.alerting, "第3次命中应触发告警");
    }

    #[tokio::test]
    async fn debounce_recovery_requires_consecutive_successes() {
        let mut engine = engine_with_rules_cfg(vec![availability_rule()], Some(1), Some(2));
        engine
            .check(&http_result(false, false, None))
            .await
            .unwrap();
        assert!(engine.alerting, "failures=1时立即告警");
        // 第1次正常：未达恢复阈值，仍处告警态
        engine
            .check(&http_result(true, true, Some(200)))
            .await
            .unwrap();
        assert!(engine.alerting);
        // 第2次连续正常：发送恢复
        engine
            .check(&http_result(true, true, Some(200)))
            .await
            .unwrap();
        assert!(!engine.alerting);
    }

    #[tokio::test]
    async fn flap_between_ok_and_hit_never_fires() {
        // 状态反复抖动时，hit/ok连续计数不断被清零，永远达不到阈值
        let mut engine = engine_with_rules_cfg(vec![availability_rule()], Some(2), Some(2));
        for _ in 0..4 {
            engine
                .check(&http_result(true, false, None))
                .await
                .unwrap();
            engine
                .check(&http_result(true, true, Some(200)))
                .await
                .unwrap();
        }
        assert!(!engine.alerting);
    }

    #[test]
    fn threshold_triggers_on_breach() {
        let engine = engine_with_rules_cfg(vec![threshold_rule("cpu", ">", 80.0)], None, None);
        let msg = engine.evaluate(&cpu_result(93.2)).expect("越限应命中告警");
        assert!(msg.contains("cpu > 80"));
        assert!(msg.contains("93.2"));
        assert!(engine.evaluate(&cpu_result(50.0)).is_none(), "未越限不应告警");
    }

    #[test]
    fn threshold_metric_mismatch_is_skipped() {
        // cpu规则配在Memory结果上：跳过而不是误判
        let engine = engine_with_rules_cfg(vec![threshold_rule("cpu", ">", 1.0)], None, None);
        let mem = CheckResult {
            id: 6,
            monitor_type: MonitorType::Memory,
            target: None,
            status: true,
            details: CheckResultDetail::Memory(MemoryMonitorResult {
                total_bytes: 100,
                used_bytes: 99,
                usage_percent: 99.0,
            }),
        };
        assert!(engine.evaluate(&mem).is_none());
    }

    #[test]
    fn threshold_skipped_when_task_failed() {
        let engine = engine_with_rules_cfg(vec![threshold_rule("cpu", ">", 1.0)], None, None);
        let mut failed = cpu_result(93.2);
        failed.status = false;
        assert!(engine.evaluate(&failed).is_none());
    }

    #[test]
    fn disk_threshold_computes_usage() {
        let engine = engine_with_rules_cfg(vec![threshold_rule("disk", ">=", 90.0)], None, None);
        let disk = CheckResult {
            id: 7,
            monitor_type: MonitorType::Disk,
            target: None,
            status: true,
            details: CheckResultDetail::Disk(DiskMonitorResult {
                total_bytes: 1000,
                available_bytes: 50, // 使用率 = 95%
                disks: vec![],
            }),
        };
        assert!(engine.evaluate(&disk).is_some());
    }

    #[test]
    fn threshold_missing_condition_is_skipped() {
        // THRESHOLD规则未配置threshold条件：跳过（API侧会拦，这里防御JSON直写库）
        let engine = engine_with_rules_cfg(
            vec![AlertSingleRule {
                rule_type: AlertRuleTypes::Threshold,
                condition: NotifyCondition::default(),
            }],
            None,
            None,
        );
        assert!(engine.evaluate(&cpu_result(99.0)).is_none());
    }
}
