// 告警/通知配置族：告警规则、触发条件、通知渠道配置，以及渠道类型（NotifyType）
// 与阈值比较符（CompareOp）两个"字符串合法值唯一来源"枚举。

/// 告警规则类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AlertRuleTypes {
    #[serde(rename = "RESPONSE_CODE")]
    ResponseCode, // 响应码规则（仅HTTP）
    #[serde(rename = "CONTENT")]
    Content, // 内容匹配规则（仅HTTP：内容校验规则存在未命中项时告警）
    #[serde(rename = "AVAILABILITY")]
    Availability, // 可用性规则（全部监控类型生效）
    #[serde(rename = "THRESHOLD")]
    Threshold, // 阈值规则（系统资源类：CPU/内存/磁盘/进程）
}

/// 告警触发条件
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct NotifyCondition {
    /// 允许的响应码白名单：响应码不在此列表内则触发告警（原名 no_contains）
    #[serde(default, alias = "no_contains")]
    pub allowed_codes: Vec<u16>,
    /// 异常响应码黑名单：响应码在此列表内则触发告警（原名 contains）
    #[serde(default, alias = "contains")]
    pub deny_codes: Vec<u16>,
    #[serde(default)]
    pub regex: String, // 响应码正则匹配
    /// 阈值条件（THRESHOLD规则用）：如 {"metric":"cpu","op":">","value":80}
    #[serde(default)]
    pub threshold: Option<ThresholdCondition>,
}

/// 阈值条件：metric为资源类别，op为比较符，value为阈值
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ThresholdCondition {
    pub metric: String, // cpu / memory / disk（使用率）/ available_bytes / process
    pub op: String,     // > >= < <= ==（合法性与比较语义集中在 CompareOp）
    pub value: f64,     // 阈值
}

/// 阈值比较符：合法值集合与比较语义的唯一来源。
/// 保持 ThresholdCondition.op 为 String（兼容库中既有 config_json，不改变反序列化行为），
/// 校验（validation）与判定（rules::evaluate_threshold）都经此枚举，消除此前散落两处的
/// 字符串字面量 match。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompareOp {
    Gt,
    Ge,
    Lt,
    Le,
    Eq,
}

impl CompareOp {
    /// 解析比较符；非法值返回 None（"=" 视作 "=="）
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            ">" => Some(CompareOp::Gt),
            ">=" => Some(CompareOp::Ge),
            "<" => Some(CompareOp::Lt),
            "<=" => Some(CompareOp::Le),
            "==" | "=" => Some(CompareOp::Eq),
            _ => None,
        }
    }

    /// 判定 lhs op rhs 是否成立（Eq 用 f64::EPSILON 容差）
    pub fn apply(&self, lhs: f64, rhs: f64) -> bool {
        match self {
            CompareOp::Gt => lhs > rhs,
            CompareOp::Ge => lhs >= rhs,
            CompareOp::Lt => lhs < rhs,
            CompareOp::Le => lhs <= rhs,
            CompareOp::Eq => (lhs - rhs).abs() < f64::EPSILON,
        }
    }
}

/// 告警配置：通知类型 + 通知渠道 + 告警触发规则列表
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AlertVerificationRules {
    pub notify_type: String, // 通知渠道：FEISHU/DINGTALK/WECOM/EMAIL（大小写不敏感，经 NotifyType::parse 解析）
    pub notify_config: NotifyConfig, // 通知渠道配置
    #[serde(default)]
    pub rules: Vec<AlertSingleRule>, // 告警触发规则列表
    /// 告警防抖：连续 N 次命中规则才发送告警（缺省1=首次命中即告警；根治网络抖动误报）
    #[serde(default)]
    pub consecutive_failures: Option<u32>,
    /// 恢复防抖：连续 M 次正常才发送恢复通知（缺省1；防止单次成功误报恢复）
    #[serde(default)]
    pub consecutive_successes: Option<u32>,
}

/// webhook_url 占位符片段：示例配置里未替换真实地址时携带此片段，
/// 调度器加载配置时据此提前告警（避免真正触发告警时才发现发不出去）。
pub const WEBHOOK_PLACEHOLDER: &str = "you/to/path";

/// 告警通知渠道配置
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct NotifyConfig {
    /// Webhook地址：FEISHU/DINGTALK/WECOM渠道必填（validate_config校验非空），
    /// EMAIL渠道不使用——serde(default)使其无需填占位值
    #[serde(default)]
    pub webhook_url: String,
    /// 签名密钥（可选）：FEISHU/DINGTALK 后台开启"签名校验"时必填，
    /// 引擎自动计算签名——飞书附加到请求体，钉钉自动拼接到 webhook_url
    #[serde(default)]
    pub secret: Option<String>,
    /// SMTP邮件配置（可选）：notify_type=EMAIL 时必填，其余渠道忽略
    #[serde(default)]
    pub email: Option<EmailNotifyConfig>,
}

/// EMAIL渠道的SMTP配置：直连SMTP服务器发信（465隐式TLS / 587 STARTTLS / 25明文）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EmailNotifyConfig {
    pub smtp_host: String, // SMTP服务器地址，如 smtp.example.com
    #[serde(default = "default_smtp_port")]
    pub smtp_port: u16, // 465=隐式TLS，587=STARTTLS，25=明文
    pub username: String,  // 认证用户名（通常为发件邮箱）
    #[serde(default)]
    pub password: String, // 认证密码/授权码；为空则跳过认证（本地relay场景）
    #[serde(default)]
    pub from: Option<String>, // 发件人；缺省使用username
    pub to: Vec<String>,   // 收件人列表（至少一个）
}

fn default_smtp_port() -> u16 {
    465
}

/// 单条告警触发规则
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AlertSingleRule {
    pub rule_type: AlertRuleTypes, // 规则类型：RESPONSE_CODE/CONTENT/AVAILABILITY/THRESHOLD
    pub condition: NotifyCondition, // 触发条件
}

/// 通知渠道类型：字符串 notify_type 的唯一事实来源。
/// 保持配置里 notify_type 为字符串（大小写不敏感、兼容库中既有 config_json），
/// 但发送分发与配置校验都经由本枚举解析，消除散落的字符串字面量匹配与拼写漂移。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifyType {
    Feishu,
    Dingtalk,
    Wecom,
    Email,
    Sms,
}

impl NotifyType {
    /// 大小写不敏感解析；无法识别的渠道返回 None（发送层告警、校验层拒绝）
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_uppercase().as_str() {
            "FEISHU" => Some(NotifyType::Feishu),
            "DINGTALK" => Some(NotifyType::Dingtalk),
            "WECOM" => Some(NotifyType::Wecom),
            "EMAIL" => Some(NotifyType::Email),
            "SMS" => Some(NotifyType::Sms),
            _ => None,
        }
    }
}

#[cfg(test)]
mod notify_type_tests {
    use super::NotifyType;

    #[test]
    fn parse_is_case_insensitive_and_trims() {
        assert_eq!(NotifyType::parse("FEISHU"), Some(NotifyType::Feishu));
        assert_eq!(NotifyType::parse("feishu"), Some(NotifyType::Feishu));
        assert_eq!(
            NotifyType::parse("  DingTalk  "),
            Some(NotifyType::Dingtalk)
        );
        assert_eq!(NotifyType::parse("wecom"), Some(NotifyType::Wecom));
        assert_eq!(NotifyType::parse("Email"), Some(NotifyType::Email));
        assert_eq!(NotifyType::parse("SMS"), Some(NotifyType::Sms));
        assert_eq!(NotifyType::parse("telegram"), None);
        assert_eq!(NotifyType::parse(""), None);
    }
}

#[cfg(test)]
mod compare_op_tests {
    use super::CompareOp;

    #[test]
    fn parse_accepts_all_legal_ops_and_alias() {
        assert_eq!(CompareOp::parse(">"), Some(CompareOp::Gt));
        assert_eq!(CompareOp::parse(">="), Some(CompareOp::Ge));
        assert_eq!(CompareOp::parse("<"), Some(CompareOp::Lt));
        assert_eq!(CompareOp::parse("<="), Some(CompareOp::Le));
        assert_eq!(CompareOp::parse("=="), Some(CompareOp::Eq));
        assert_eq!(CompareOp::parse("="), Some(CompareOp::Eq), "= 是 == 的别名");
        assert_eq!(CompareOp::parse("~"), None);
        assert_eq!(CompareOp::parse(""), None);
    }

    #[test]
    fn apply_matches_operator_semantics() {
        assert!(CompareOp::Gt.apply(90.0, 80.0));
        assert!(!CompareOp::Gt.apply(80.0, 80.0));
        assert!(CompareOp::Ge.apply(80.0, 80.0));
        assert!(CompareOp::Lt.apply(5.0, 10.0));
        assert!(CompareOp::Le.apply(10.0, 10.0));
        assert!(CompareOp::Eq.apply(50.0, 50.0));
        assert!(!CompareOp::Eq.apply(50.0, 50.1));
    }
}
