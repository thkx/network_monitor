// 通知引擎：根据通知类型把告警消息发送到对应的渠道
// 支持渠道：FEISHU（可选secret签名）、DINGTALK、WECOM（EMAIL/SMS为占位）
// 发送失败自动转入后台退避重试（见 retry_with_backoff / RETRY_DELAYS_SECS）

use crate::tools_types::NotifyConfig;
use reqwest::Client;
use std::time::Duration;

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
                tracing::info!("[EMAIL告警] {}", message);
            }
            "SMS" => {
                // 短信通知（预留）
                tracing::info!("[SMS告警] {}", message);
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
                tracing::warn!("未知的告警通知类型: {}", self.notify_type);
            }
        }
    }

    // 统一的webhook POST：带5秒超时，避免通知渠道故障阻塞监控任务循环
    // 首次发送失败后转入后台退避重试：告警因网络抖动被静默丢弃的代价太高
    async fn post_webhook(&self, body: serde_json::Value, message: &str, channel: &str) {
        let client = match Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("{}告警发送失败，创建HTTP客户端异常: {}", channel, e);
                return;
            }
        };
        match Self::try_post(&client, &self.notify_config.webhook_url, &body, channel).await {
            Ok(()) => {
                tracing::info!("{}告警发送成功: {}", channel, message);
                return;
            }
            Err(e) => {
                tracing::error!(
                    "{}告警发送失败: {}（将后台重试{}次，请检查notify_config中的webhook_url是否有效）",
                    channel,
                    e,
                    RETRY_DELAYS_SECS.len()
                );
            }
        }
        // 后台重试：全部字段克隆进任务，不阻塞监控任务循环；
        // 重试成功/耗尽都有明确日志，最终失败时告警丢失是显式可见的
        let url = self.notify_config.webhook_url.clone();
        let channel = channel.to_string();
        let message = message.to_string();
        tokio::spawn(async move {
            let outcome = retry_with_backoff(
                |attempt| {
                    let client = client.clone();
                    let body = body.clone();
                    let url = url.clone();
                    let channel = channel.clone();
                    async move { Self::try_post(&client, &url, &body, &channel).await.map_err(|e| format!("第{}次重试: {}", attempt + 1, e)) }
                },
                &RETRY_DELAYS_SECS,
            )
            .await;
            match outcome {
                Ok(()) => tracing::info!("{}告警重试成功: {}", channel, message),
                Err(e) => tracing::error!("{}告警重试全部失败，通知可能丢失: {}（{}）", channel, message, e),
            }
        });
    }

    // 单次webhook发送尝试：2xx视为成功，其余状态码与请求错误均为失败
    pub(crate) async fn try_post(
        client: &Client,
        url: &str,
        body: &serde_json::Value,
        channel: &str,
    ) -> Result<(), String> {
        match client.post(url).json(body).send().await {
            Ok(resp) => {
                if resp.status().is_success() {
                    Ok(())
                } else {
                    Err(format!("{}返回状态码 {}", channel, resp.status()))
                }
            }
            Err(e) => Err(format!("请求错误 {}", e)),
        }
    }
}

// 重试退避策略：失败后1s/5s/30s/2m/5m各重试一次（共5次重试+1次首发）
pub(crate) const RETRY_DELAYS_SECS: [u64; 5] = [1, 5, 30, 120, 300];

// 通用退避重试循环：对attempt调用至多delays.len()次，每次前等待对应秒数
// 返回Ok=某次尝试成功；Err=全部耗尽（附带最后一次错误）
pub(crate) async fn retry_with_backoff<F, Fut>(mut attempt: F, delays: &[u64]) -> Result<(), String>
where
    F: FnMut(usize) -> Fut,
    Fut: std::future::Future<Output = Result<(), String>>,
{
    let mut last_err = String::from("未执行任何尝试");
    for (i, &delay) in delays.iter().enumerate() {
        if delay > 0 {
            tokio::time::sleep(Duration::from_secs(delay)).await;
        }
        match attempt(i).await {
            Ok(()) => return Ok(()),
            Err(e) => {
                tracing::warn!("通知第{}次重试失败: {}", i + 1, e);
                last_err = e;
            }
        }
    }
    Err(last_err)
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
    use super::{NotifyEngine, RETRY_DELAYS_SECS, feishu_sign, retry_with_backoff};

    #[test]
    fn feishu_sign_matches_reference_vector() {
        // 参考向量由PowerShell HMACSHA256独立计算：key = "1712126400\ntest-secret"，消息为空
        assert_eq!(
            feishu_sign(1712126400, "test-secret"),
            "BbGyLWSeiDDaf5+1ZZHc+miQjKW0aT9Rp/4iOd3ju5I="
        );
    }

    #[tokio::test]
    async fn retry_succeeds_after_transient_failures() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let calls = std::sync::Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        // 前两次失败、第三次成功：退避循环应停在那个Ok
        let result = retry_with_backoff(
            move |_| {
                let n = counter.fetch_add(1, Ordering::SeqCst) + 1;
                async move {
                    if n < 3 {
                        Err(format!("fail {n}"))
                    } else {
                        Ok(())
                    }
                }
            },
            &[0, 0, 0],
        )
        .await;
        assert!(result.is_ok(), "第3次尝试应成功");
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn retry_reports_err_when_exhausted() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let calls = std::sync::Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let result = retry_with_backoff(
            move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
                async move { Err::<(), String>("down".to_string()) }
            },
            &[0, 0],
        )
        .await;
        assert!(result.is_err());
        // delays长度即重试次数（首发在retry之外）
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn retry_delays_are_strictly_increasing_backoff() {
        assert!(
            RETRY_DELAYS_SECS.windows(2).all(|w| w[0] < w[1]),
            "退避间隔应严格递增"
        );
        assert_eq!(RETRY_DELAYS_SECS.first(), Some(&1), "首次重试1秒后");
    }

    #[tokio::test]
    async fn try_post_fails_on_refused_connection() {
        use std::time::Duration;
        use reqwest::Client;
        // 连接被拒（端口9）时try_post应快速返回Err，且错误信息含上下文
        let client = Client::builder()
            .timeout(Duration::from_millis(500))
            .build()
            .unwrap();
        let body = serde_json::json!({"msg_type": "text", "content": {"text": "t"}});
        let r = NotifyEngine::try_post(&client, "http://127.0.0.1:9/hook", &body, "飞书").await;
        let err = r.expect_err("端口9应连接失败");
        assert!(err.contains("请求错误"), "错误应标注来源: {err}");
    }
}
