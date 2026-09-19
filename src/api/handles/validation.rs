// 创建/更新监控前的配置校验：非法配置返回 400，避免脏配置入库
// （从 monitor_handlers 抽出：校验规则独立演进，与 handler 编排逻辑解耦）
use regex::Regex;

use crate::tools_types::{AlertRuleTypes, ContentVerificationRules, SelfDefineMonitorConfig};

// 创建/更新前的配置校验：非法配置返回400，避免脏配置入库
pub fn validate_config(entry: &SelfDefineMonitorConfig) -> Result<(), actix_web::Error> {
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
    // 告警配置校验：防抖参数范围 + 通知渠道专项 + THRESHOLD规则的阈值条件
    if let Some(cfg) = entry.alert_rules.as_ref() {
        // 渠道专项校验：EMAIL必须带SMTP配置且收件人非空；SMS未实现，显式拒绝；
        // webhook渠道URL必填——避免"接受了配置却永远发送失败重试"的无效配置
        match cfg.notify_type.to_uppercase().as_str() {
            "EMAIL" => {
                let Some(email) = cfg.notify_config.email.as_ref() else {
                    return Err(actix_web::error::ErrorBadRequest(
                        "EMAIL 通知必须配置 notify_config.email（smtp_host/username/password/to）",
                    ));
                };
                if email.to.is_empty() {
                    return Err(actix_web::error::ErrorBadRequest(
                        "EMAIL 通知的 email.to 至少需要一位收件人",
                    ));
                }
            }
            "SMS" => {
                return Err(actix_web::error::ErrorBadRequest(
                    "SMS 通知暂未实现，请使用 FEISHU / DINGTALK / WECOM / EMAIL",
                ));
            }
            "FEISHU" | "DINGTALK" | "WECOM"
                if cfg.notify_config.webhook_url.trim().is_empty() =>
            {
                return Err(actix_web::error::ErrorBadRequest(
                    "webhook 渠道必须配置 notify_config.webhook_url",
                ));
            }
            _ => {}
        }
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

#[cfg(test)]
mod tests {
    use super::validate_config;
    use crate::tools_types::SelfDefineMonitorConfig;

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
}
