// 创建/更新监控前的配置校验：非法配置返回 400，避免脏配置入库
// （从 monitor_handlers 抽出：校验规则独立演进，与 handler 编排逻辑解耦）
use regex::Regex;

use crate::domain::{
    AlertRuleTypes, ContentVerificationRules, NotifyType, MonitorDefinition,
};

// 创建/更新前的配置校验：非法配置返回400，避免脏配置入库
pub fn validate_config(entry: &MonitorDefinition) -> Result<(), actix_web::Error> {
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
        // webhook渠道URL必填——避免"接受了配置却永远发送失败重试"的无效配置。
        // 未知渠道直接拒绝：接受一个发不出去的渠道等于埋雷（此前 _ 分支静默放行）
        let Some(notify_type) = NotifyType::parse(&cfg.notify_type) else {
            return Err(actix_web::error::ErrorBadRequest(format!(
                "未知的通知渠道 {:?}，支持 FEISHU / DINGTALK / WECOM / EMAIL",
                cfg.notify_type
            )));
        };
        match notify_type {
            NotifyType::Email => {
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
            NotifyType::Sms => {
                return Err(actix_web::error::ErrorBadRequest(
                    "SMS 通知暂未实现，请使用 FEISHU / DINGTALK / WECOM / EMAIL",
                ));
            }
            NotifyType::Feishu | NotifyType::Dingtalk | NotifyType::Wecom => {
                if cfg.notify_config.webhook_url.trim().is_empty() {
                    return Err(actix_web::error::ErrorBadRequest(
                        "webhook 渠道必须配置 notify_config.webhook_url",
                    ));
                }
            }
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
                if crate::domain::CompareOp::parse(&th.op).is_none() {
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
    use crate::domain::MonitorDefinition;

    fn entry_from_json(json: &str) -> MonitorDefinition {
        serde_json::from_str(json).expect("测试JSON应可反序列化")
    }

    // interval 边界（表驱动）：下限0拒绝、上限86401拒绝、边界值86400接受
    #[test]
    fn interval_bounds() {
        for (interval, expect_ok) in [(0u64, false), (86401, false), (86400, true)] {
            let entry = entry_from_json(&format!(
                r#"{{"target":"https://a.com","monitor_type":"HTTP","interval":{interval}}}"#
            ));
            assert_eq!(
                validate_config(&entry).is_ok(),
                expect_ok,
                "interval={interval} 期望 ok={expect_ok}"
            );
        }
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

    // 防抖参数边界（表驱动）：failures=0 拒绝、successes=1001 越上限拒绝、3/2 合法接受
    #[test]
    fn debounce_bounds() {
        for (fields, expect_ok) in [
            (r#""consecutive_failures":0"#, false),
            (r#""consecutive_successes":1001"#, false),
            (
                r#""consecutive_failures":3,"consecutive_successes":2"#,
                true,
            ),
        ] {
            let entry = entry_from_json(&format!(
                r#"{{"target":"https://a.com","monitor_type":"HTTP","alert_rules":{{"notify_type":"FEISHU","notify_config":{{"webhook_url":"http://x"}},"rules":[],{fields}}}}}"#
            ));
            assert_eq!(
                validate_config(&entry).is_ok(),
                expect_ok,
                "debounce 字段 [{fields}] 期望 ok={expect_ok}"
            );
        }
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

    #[test]
    fn unknown_notify_type_is_rejected() {
        // 未知渠道此前走 _ 分支静默放行，接受一个发不出去的配置；现在应拒绝
        let entry = entry_from_json(
            r#"{"target":"https://a.com","monitor_type":"HTTP","alert_rules":{"notify_type":"telegram","notify_config":{"webhook_url":"http://x"},"rules":[]}}"#,
        );
        assert!(validate_config(&entry).is_err());
    }

    #[test]
    fn notify_type_is_case_insensitive() {
        // 小写 notify_type 应被 NotifyType::parse 正常识别（大小写不敏感）
        let entry = entry_from_json(
            r#"{"target":"https://a.com","monitor_type":"HTTP","alert_rules":{"notify_type":"feishu","notify_config":{"webhook_url":"http://x"},"rules":[]}}"#,
        );
        assert!(validate_config(&entry).is_ok());
    }

    #[test]
    fn webhook_channel_requires_url() {
        // WECOM 等 webhook 渠道 URL 为空应拒绝
        let entry = entry_from_json(
            r#"{"target":"https://a.com","monitor_type":"HTTP","alert_rules":{"notify_type":"WECOM","notify_config":{"webhook_url":"  "},"rules":[]}}"#,
        );
        assert!(validate_config(&entry).is_err());
    }
}
