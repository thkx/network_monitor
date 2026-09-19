// 告警引擎：当监控结果产生时，按告警规则判断是否需要告警（消息文本的构造也在这里）
// 支持的规则类型：
//   RESPONSE_CODE —— HTTP响应码规则（仅HTTP监控生效）
//   AVAILABILITY  —— 通用可用性规则（全部监控类型生效：探测不可用即触发）
//   THRESHOLD     —— 阈值规则（系统资源类：CPU/内存/磁盘/进程数值越限）
// 告警防抖：consecutive_failures 连续N次命中才告警、consecutive_successes 连续M次正常才恢复
// 通知渠道的发送与重试见 super::notify（NotifyEngine）

use super::notify::NotifyEngine;
use super::rules::{
    detail_summary, evaluate_content, evaluate_response_code, evaluate_threshold,
    is_target_available,
};
use crate::database::repositories::alert_state_repo::AlertStateRepository;
use crate::monitor::types::CheckResult;
use crate::tools_types::{AlertRuleTypes, AlertVerificationRules};

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

    // 保存抑制状态（有持久化时）；失败只打日志，不影响告警主流程。
    // diesel的SQLite写是同步阻塞调用，挪到spawn_blocking阻塞池执行，
    // 避免监控任务所在的runtime线程被DB写延迟卡住
    async fn persist_state(&self, alerting: bool) {
        let Some((repo, monitor_id)) = &self.state else {
            return;
        };
        let repo = repo.clone();
        let monitor_id = *monitor_id;
        match tokio::task::spawn_blocking(move || repo.set_alerting(monitor_id, alerting)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                tracing::error!("保存告警状态失败 (monitor_id={}): {}", monitor_id, e)
            }
            Err(e) => tracing::error!("保存告警状态任务失败 (monitor_id={}): {}", monitor_id, e),
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
                    self.persist_state(true).await;
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
                            "{} target: {} | {} | 异常已恢复（{}）",
                            super::RECOVERY_PREFIX,
                            target,
                            check_result.monitor_type,
                            detail_summary(&check_result.details)
                        ))
                        .await;
                    self.alerting = false;
                    self.ok_streak = 0;
                    self.persist_state(false).await;
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
                        evaluate_response_code(&target, check_result, &single_rule.condition)
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
                    // 内容匹配规则（仅HTTP）：内容校验存在未命中项时告警。
                    // 与AVAILABILITY正交——内容校验失败不影响可达性判定，由本规则单独负责
                    if let Some(message) = evaluate_content(&target, check_result) {
                        return Some(message);
                    }
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::AlertsEngine;
    use crate::monitor::types::{
        CheckResult, CheckResultDetail, CpuMonitorResult, DiskMonitorResult, IcmpMonitorResult,
        MemoryMonitorResult,
    };
    use crate::tools_types::{
        AlertRuleTypes, AlertSingleRule, AlertVerificationRules, BasicAvailability,
        ContentVerificationRules, HttpMonitorResult, MonitorType, NotifyCondition, NotifyConfig,
        ThresholdCondition,
    };

    // 构造HTTP类型的检查结果（其余字段走Default）
    fn http_result(status: bool, reachable: bool, code: Option<u16>) -> CheckResult {
        CheckResult {
            id: 1,
            monitor_type: MonitorType::Http,
            target: Some("https://example.com".to_string()),
            status,
            details: CheckResultDetail::Http(HttpMonitorResult {
                basic_available: BasicAvailability {
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
                email: None,
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
        // 系统资源类监控能执行即视为可用：AVAILABILITY 规则不触发
        assert!(engine_with_rules(vec![availability_rule()])
            .evaluate(&result)
            .is_none());
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

    // ---- CONTENT 内容匹配规则 ----

    fn http_with_content(failed_rules: Vec<ContentVerificationRulesResultAlias>) -> CheckResult {
        let mut result = http_result(true, true, Some(200));
        if let CheckResultDetail::Http(ref mut r) = result.details {
            r.content_verification.failed_rules = failed_rules;
        }
        result
    }

    // 别名避免直接依赖crate::tools_types的完整路径（ContentVerificationRulesResult）
    type ContentVerificationRulesResultAlias = crate::tools_types::ContentVerificationRulesResult;

    fn failed_rule(kind: ContentVerificationRules, content: &str) -> ContentVerificationRulesResultAlias {
        ContentVerificationRulesResultAlias {
            rules: crate::tools_types::ContentVerificationRulesSingle {
                rule_type: kind,
                rule_content: content.to_string(),
                rule_description: String::new(),
            },
            ..Default::default()
        }
    }

    #[test]
    fn content_rule_triggers_on_failed_verification() {
        let engine = engine_with_rules(vec![AlertSingleRule {
            rule_type: AlertRuleTypes::Content,
            condition: NotifyCondition::default(),
        }]);
        let result = http_with_content(vec![failed_rule(
            ContentVerificationRules::Contains,
            "expected-text",
        )]);
        let msg = engine.evaluate(&result).expect("内容校验失败应告警");
        assert!(msg.contains("contains(expected-text)"), "消息应含规则明细: {msg}");
        assert!(msg.contains("内容校验失败 1 项"));
    }

    #[test]
    fn content_rule_silent_when_all_matched() {
        let engine = engine_with_rules(vec![AlertSingleRule {
            rule_type: AlertRuleTypes::Content,
            condition: NotifyCondition::default(),
        }]);
        // 无失败规则（含完全通过或未配置内容规则两种情形）都应静默
        assert!(engine.evaluate(&http_result(true, true, Some(200))).is_none());
    }

    #[test]
    fn content_rule_skipped_for_non_http_details() {
        let engine = engine_with_rules(vec![AlertSingleRule {
            rule_type: AlertRuleTypes::Content,
            condition: NotifyCondition::default(),
        }]);
        let down = CheckResult {
            id: 8,
            monitor_type: MonitorType::Icmp,
            target: Some("10.0.0.1".to_string()),
            status: true,
            details: CheckResultDetail::Icmp(IcmpMonitorResult {
                is_alive: false,
                elapsed_ms: 30,
            }),
        };
        assert!(engine.evaluate(&down).is_none());
    }
}
