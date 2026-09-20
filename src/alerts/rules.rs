// 告警规则的无状态评估函数：可用性判定、内容校验、阈值判定，以及消息文本构造。
// 从 engine 抽出——这些是纯函数（输入检查结果，输出是否命中/消息），与状态机（防抖、
// 抑制状态持久化）的变化原因不同：规则语义调整不碰状态机，状态机演进不碰规则判定。
use crate::monitor::types::{CheckResult, CheckResultDetail};
use crate::tools_types::{ContentVerificationRules, HttpMonitorResult, NotifyCondition};
use regex::Regex;

// 目标是否可用：任务执行失败、或各类型结果中的可达性标志为false 都视为不可用
// 系统资源类监控（CPU/MEMORY/DISK/PROCESS）能执行即视为可用（无可用性语义，仅受任务状态影响）
pub(super) fn is_target_available(check_result: &CheckResult) -> bool {
    if !check_result.status {
        return false;
    }
    match &check_result.details {
        CheckResultDetail::Http(r) => r.basic_available.is_reachable,
        CheckResultDetail::Icmp(r) => r.is_alive,
        CheckResultDetail::Tcp(r) => r.connected,
        CheckResultDetail::Udp(r) => r.response_received,
        CheckResultDetail::Dns(r) => r.resolved,
        CheckResultDetail::Ftp(r) => r.available(),
        CheckResultDetail::Traceroute(r) => r.success,
        CheckResultDetail::Cpu(_)
        | CheckResultDetail::Memory(_)
        | CheckResultDetail::Disk(_)
        | CheckResultDetail::Process(_) => true,
        CheckResultDetail::Unknown(_) => false,
    }
}

// RESPONSE_CODE规则评估（仅HTTP监控有意义）：命中条件返回告警消息
pub(super) fn evaluate_response_code(
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
    let Some(code) = http_result.basic_available.res_status_code else {
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
        && regex_r.is_match(&code.to_string())
    {
        return Some(format!(
            "[监控告警] target: {} | 响应码{}命中正则{}",
            target, code, regex
        ));
    }
    None
}

// 内容匹配规则评估（仅HTTP有意义）：内容校验存在未命中项时返回告警消息
// 依赖HTTP监控已执行的content_evaluation_rules结果——监控本身未配置内容规则时，
// failed_rules恒为空，本规则静默跳过（不会把"没配规则"当成"内容异常"）
pub(super) fn evaluate_content(target: &str, check_result: &CheckResult) -> Option<String> {
    let CheckResultDetail::Http(ref http_result) = check_result.details else {
        return None;
    };
    let failed = &http_result.content_verification.failed_rules;
    if failed.is_empty() {
        return None;
    }
    let names: Vec<String> = failed
        .iter()
        .map(|r| {
            let type_name = match r.rules.rule_type {
                ContentVerificationRules::Contains => "contains",
                ContentVerificationRules::NotContains => "not_contains",
                ContentVerificationRules::Regex => "regex",
                ContentVerificationRules::Default => "default",
            };
            format!("{}({})", type_name, r.rules.rule_content)
        })
        .collect();
    Some(format!(
        "[监控告警] target: {} | 内容校验失败 {} 项: {}",
        target,
        failed.len(),
        names.join("、")
    ))
}

// 阈值规则评估（CPU/内存/磁盘/进程有意义）：数值越限返回告警消息
// 任务执行失败时跳过（此时数值不可信，失败场景已由AVAILABILITY规则覆盖）
pub(super) fn evaluate_threshold(
    check_result: &CheckResult,
    condition: &NotifyCondition,
) -> Option<String> {
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
pub(super) fn detail_summary(details: &CheckResultDetail) -> String {
    match details {
        CheckResultDetail::Http(r) => match r.basic_available.res_status_code {
            Some(code) => format!("状态码 {}，耗时 {} ms", code, r.performance_timings.total_time),
            None => format!("请求失败（{}）", failure_reason(r)),
        },
        // 有真实链路 RTT 时优先展示（含总耗时便于对比进程开销）；否则只报墙钟耗时
        CheckResultDetail::Icmp(r) => match r.rtt_ms {
            Some(rtt) => format!(
                "存活 {}，RTT {:.2} ms（探测总耗时 {} ms）",
                r.is_alive, rtt, r.elapsed_ms
            ),
            None => format!("存活 {}，探测耗时 {} ms", r.is_alive, r.elapsed_ms),
        },
        CheckResultDetail::Tcp(r) => format!("连接 {}，耗时 {} ms", r.connected, r.elapsed_ms),
        CheckResultDetail::Udp(r) => format!(
            "响应 {}（{}），耗时 {} ms",
            r.response_received,
            if r.dns_mode { "DNS语义" } else { "通用回包" },
            r.elapsed_ms
        ),
        CheckResultDetail::Dns(r) => format!("解析 {}，耗时 {} ms", r.resolved, r.elapsed_ms),
        CheckResultDetail::Ftp(r) => format!(
            "连接 {}，握手 {}，匿名登录 {}（最后响应 {}），耗时 {} ms",
            r.connected,
            r.handshake_ok,
            r.logged_in,
            r.last_code.map(|c| c.to_string()).unwrap_or_else(|| "-".to_string()),
            r.elapsed_ms
        ),
        CheckResultDetail::Traceroute(r) => format!("成功 {}，{} 跳", r.success, r.hops.len()),
        CheckResultDetail::Cpu(r) => format!("使用率 {:.1}%", r.usage_percent),
        CheckResultDetail::Memory(r) => format!("使用率 {:.1}%", r.usage_percent),
        CheckResultDetail::Disk(r) => format!("剩余可用 {} MB", r.available_bytes / 1024 / 1024),
        CheckResultDetail::Process(r) => format!("进程数 {}", r.process_count),
        CheckResultDetail::Unknown(_) => "未知结果".to_string(),
    }
}

// HTTP失败原因：优先使用 error_kind/error_message（增强前产生的旧记录可能为空）
pub(super) fn failure_reason(r: &HttpMonitorResult) -> String {
    match (&r.error_kind, &r.error_message) {
        (Some(kind), Some(msg)) => format!("{}: {}", kind, msg),
        (Some(kind), None) => kind.clone(),
        (None, Some(msg)) => msg.clone(),
        (None, None) => "未知原因".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{detail_summary, failure_reason, is_target_available};
    use crate::monitor::types::{
        CheckResult, CheckResultDetail, IcmpMonitorResult,
    };
    use crate::tools_types::{BasicAvailability, HttpMonitorResult, MonitorType};

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

    #[test]
    fn failed_task_is_unavailable() {
        assert!(!is_target_available(&http_result(false, true, Some(200))));
        assert!(is_target_available(&http_result(true, true, Some(200))));
        assert!(!is_target_available(&http_result(true, false, None)));
    }

    #[test]
    fn availability_applies_to_icmp() {
        let down = CheckResult {
            id: 3,
            monitor_type: MonitorType::Icmp,
            target: Some("10.0.0.1".to_string()),
            status: true,
            details: CheckResultDetail::Icmp(IcmpMonitorResult {
                is_alive: false,
                elapsed_ms: 30,
                rtt_ms: None,
            }),
        };
        assert!(!is_target_available(&down));
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
