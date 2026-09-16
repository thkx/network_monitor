// 告警引擎模块：当监控结果产生时，按告警规则判断是否需要告警，并通过通知渠道发送消息
// 支持的规则类型：
//   RESPONSE_CODE —— HTTP响应码规则（仅HTTP监控生效）
//   AVAILABILITY  —— 通用可用性规则（全部监控类型生效：探测不可用即触发）
// 支持的通知渠道：FEISHU（可选secret签名）、DINGTALK、WECOM（EMAIL/SMS为占位）

use crate::database::repositories::alert_state_repo::AlertStateRepository;
use crate::monitor::types::{CheckResult, CheckResultDetail};
use crate::tools_types::{
    AlertRuleTypes, AlertVerificationRules, HttpMonitorResult, NotifyCondition, NotifyConfig,
};
use regex::Regex;
use reqwest::Client;
use std::time::Duration;

// 告警引擎：持有通知引擎、告警规则与抑制状态，对外提供统一的check入口
pub struct AlertsEngine {
    notify: NotifyEngine,                // 通知引擎
    alert_rules: AlertVerificationRules, // 告警规则配置
    // 告警抑制状态：是否处于"已告警、未恢复"状态（防止同一故障反复轰炸通知渠道）
    alerting: bool,
    // 抑制状态持久化（Server模式）：重启后从 alert_state 表恢复，
    // 避免"故障还在却重复告警"、"恢复时误报"；Once/Monitor模式为None（仅内存态）
    state: Option<(AlertStateRepository, i32)>,
}

impl AlertsEngine {
    // 无持久化：Once/Monitor模式使用（没有DB主键，抑制状态仅存内存）
    pub fn new(alert_rule: AlertVerificationRules) -> Self {
        AlertsEngine {
            alerting: false,
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
            Err(e) => eprintln!("恢复告警状态失败 (monitor_id={}): {}", monitor_id, e),
        }
        engine.state = Some((repo, monitor_id));
        engine
    }

    // 保存抑制状态（有持久化时）；失败只打日志，不影响告警主流程
    fn persist_state(&self, alerting: bool) {
        if let Some((repo, monitor_id)) = &self.state
            && let Err(e) = repo.set_alerting(*monitor_id, alerting)
        {
            eprintln!("保存告警状态失败 (monitor_id={}): {}", monitor_id, e);
        }
    }

    // 告警规则check事件：状态机式告警——异常时只告警一次，恢复时发送一次恢复通知
    // 注意：失败（不可用、未拿到响应码）的监控结果同样参与检查，避免站点宕机时漏告警
    pub async fn check(&mut self, check_result: &CheckResult) -> Result<(), String> {
        match self.evaluate(check_result) {
            // 命中规则：仅在"未告警"状态下发送一次，之后抑制重复告警
            Some(message) => {
                if !self.alerting {
                    self.notify.send_alert_message(message).await;
                    self.alerting = true;
                    self.persist_state(true);
                }
            }
            // 未命中任何规则视为正常：从告警状态恢复时发送一次恢复通知
            None => {
                if self.alerting {
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

// 通知引擎：根据通知类型把告警消息发送到对应的渠道
pub struct NotifyEngine {
    notify_type: String,         // 通知类型 feishu dingtalk wecom email sms 等
    notify_config: NotifyConfig, // 通知配置内容 webhook地址、签名密钥等
}

impl NotifyEngine {
    pub fn new(notify_type: String, notify_config: NotifyConfig) -> Self {
        NotifyEngine {
            notify_type,
            notify_config,
        }
    }

    // 根据通知方式发送告警消息（类型不区分大小写）
    pub async fn send_alert_message(&self, message: String) {
        match self.notify_type.to_uppercase().as_str() {
            "EMAIL" => {
                // 邮件通知（预留）
                println!("[EMAIL告警] {}", message);
            }
            "SMS" => {
                // 短信通知（预留）
                println!("[SMS告警] {}", message);
            }
            "FEISHU" => {
                // 飞书自定义机器人：msg_type/content结构 + 可选签名
                let mut body = serde_json::json!({
                    "msg_type": "text",
                    "content": { "text": message }
                });
                // 配置了secret时追加签名与时间戳（飞书后台开启"签名校验"的机器人必须带）
                if let Some(secret) = &self.notify_config.secret {
                    let timestamp = chrono::Utc::now().timestamp();
                    body["timestamp"] = serde_json::Value::from(timestamp.to_string());
                    body["sign"] = serde_json::Value::from(feishu_sign(timestamp, secret));
                }
                self.post_webhook(body, &message, "飞书").await;
            }
            "DINGTALK" | "WECOM" => {
                // 钉钉/企业微信机器人：JSON结构相同 {"msgtype":"text","text":{"content":...}}
                // 注意：钉钉若开启"加签"安全设置，需在webhook_url自行拼接timestamp/sign参数
                let body = serde_json::json!({
                    "msgtype": "text",
                    "text": { "content": message }
                });
                let channel = if self.notify_type.to_uppercase() == "DINGTALK" {
                    "钉钉"
                } else {
                    "企业微信"
                };
                self.post_webhook(body, &message, channel).await;
            }
            _ => {
                println!("未知的告警通知类型: {}", self.notify_type);
            }
        }
    }

    // 统一的webhook POST：带5秒超时，避免通知渠道故障阻塞监控任务循环
    async fn post_webhook(&self, body: serde_json::Value, message: &str, channel: &str) {
        let client = match Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                eprintln!("{}告警发送失败，创建HTTP客户端异常: {}", channel, e);
                return;
            }
        };
        match client
            .post(&self.notify_config.webhook_url)
            .json(&body)
            .send()
            .await
        {
            Ok(resp) => {
                if resp.status().is_success() {
                    println!("{}告警发送成功: {}", channel, message);
                } else {
                    eprintln!("{}告警发送失败，状态码: {}", channel, resp.status());
                }
            }
            Err(e) => eprintln!(
                "{}告警发送失败: {}（请检查notify_config中的webhook_url是否有效）",
                channel, e
            ),
        }
    }
}

// 飞书签名算法：以 "{timestamp}\n{secret}" 为HMAC-SHA256密钥，对空消息签名后base64
fn feishu_sign(timestamp: i64, secret: &str) -> String {
    use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
    use base64::Engine as _;
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    let string_to_sign = format!("{}\n{}", timestamp, secret);
    let mut mac = Hmac::<Sha256>::new_from_slice(string_to_sign.as_bytes())
        .expect("HMAC-SHA256 accepts keys of any length");
    mac.update(b"");
    BASE64_STANDARD.encode(mac.finalize().into_bytes())
}

#[cfg(test)]
mod tests {
    use super::{detail_summary, failure_reason, feishu_sign, is_target_available, AlertsEngine};
    use crate::monitor::types::{CheckResult, CheckResultDetail, IcmpMonitorResult};
    use crate::tools_types::{
        AlertRuleTypes, AlertSingleRule, AlertVerificationRules, BasicAvailability,
        HttpMonitorResult, MonitorType, NotifyCondition, NotifyConfig,
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

    fn engine_with_rules(rules: Vec<AlertSingleRule>) -> AlertsEngine {
        AlertsEngine::new(AlertVerificationRules {
            notify_type: "FEISHU".to_string(),
            notify_config: NotifyConfig {
                webhook_url: "http://127.0.0.1:9".to_string(),
                secret: None,
            },
            rules,
        })
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
            },
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
    fn feishu_sign_matches_reference_vector() {
        // 参考向量由PowerShell HMACSHA256独立计算：key = "1712126400\ntest-secret"，消息为空
        assert_eq!(
            feishu_sign(1712126400, "test-secret"),
            "BbGyLWSeiDDaf5+1ZZHc+miQjKW0aT9Rp/4iOd3ju5I="
        );
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
}
