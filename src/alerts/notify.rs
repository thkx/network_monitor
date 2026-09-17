// 通知引擎：根据通知类型把告警消息发送到对应的渠道
// 支持渠道：FEISHU（可选secret签名）、DINGTALK（可选secret自动加签）、WECOM、EMAIL（SMTP）
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
                // SMTP邮件：构建传输器后整个会话后台化——SMTP多次往返可能远超5秒，
                // 不能阻塞监控任务循环；重试/成功/失败日志与webhook渠道同一口径
                let Some(email_cfg) = &self.notify_config.email else {
                    tracing::error!(
                        "EMAIL告警发送失败: notify_config.email 未配置（需smtp_host/username/to）"
                    );
                    return;
                };
                let cfg = email_cfg.clone();
                // 主题按消息性质区分：恢复通知与告警在邮箱里一眼可分
                let subject = if message.starts_with("[监控恢复]") {
                    "网络监控恢复通知"
                } else {
                    "网络监控告警通知"
                };
                tokio::spawn(async move {
                    let outcome = retry_with_backoff(
                        |attempt| {
                            let cfg = cfg.clone();
                            let body = message.clone();
                            async move {
                                crate::tools::send_mail(&cfg, subject, &body)
                                    .await
                                    .map_err(|e| format!("第{}次重试: {}", attempt + 1, e))
                            }
                        },
                        &RETRY_DELAYS_SECS,
                    )
                    .await;
                    match outcome {
                        Ok(()) => tracing::info!("EMAIL告警发送成功: {}", message),
                        Err(e) => tracing::error!(
                            "EMAIL告警重试全部失败，通知可能丢失: {}（{}）",
                            message,
                            e
                        ),
                    }
                });
            }
            "SMS" => {
                // 未实现的渠道显式报错而非假装成功：配置校验层已拒绝SMS，
                // 此日志只在配置绕过校验直写数据库时出现
                tracing::error!("SMS告警渠道未实现，通知未发送: {}", message);
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
                self.post_webhook(
                    self.notify_config.webhook_url.as_str(),
                    body,
                    &message,
                    "飞书",
                )
                .await;
            }
            "DINGTALK" | "WECOM" => {
                // 钉钉/企业微信机器人：JSON结构相同 {"msgtype":"text","text":{"content":...}}
                let body = serde_json::json!({
                    "msgtype": "text",
                    "text": { "content": message }
                });
                let channel = if self.notify_type.to_uppercase() == "DINGTALK" {
                    "钉钉"
                } else {
                    "企业微信"
                };
                // 钉钉"加签"安全设置：sign = base64(HMAC-SHA256(key=secret, "{毫秒时间戳}\n{secret}"))，
                // URL编码后与timestamp一起拼接到webhook_url（URL已含timestamp参数则尊重手拼结果）
                let mut url = self.notify_config.webhook_url.clone();
                if self.notify_type.to_uppercase() == "DINGTALK"
                    && let Some(secret) = &self.notify_config.secret
                    && !url.contains("timestamp=")
                {
                    let ts = chrono::Utc::now().timestamp_millis();
                    let sign = dingtalk_sign(ts, secret);
                    let encoded = url::form_urlencoded::Serializer::new(String::new())
                        .append_pair("sign", &sign)
                        .finish();
                    let sep = if url.contains('?') { "&" } else { "?" };
                    url = format!("{url}{sep}timestamp={ts}&{encoded}");
                }
                self.post_webhook(url.as_str(), body, &message, channel).await;
            }
            _ => {
                tracing::warn!("未知的告警通知类型: {}", self.notify_type);
            }
        }
    }

    // 统一的webhook POST：带5秒超时，避免通知渠道故障阻塞监控任务循环
    // 首次发送失败后转入后台退避重试：告警因网络抖动被静默丢弃的代价太高
    async fn post_webhook(&self, url: &str, body: serde_json::Value, message: &str, channel: &str) {
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
        match Self::try_post(&client, url, &body, channel).await {
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
        let url = url.to_string();
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

    // 单次webhook发送尝试：HTTP 2xx 且业务码为成功才算成功
    // 飞书/钉钉/企微的业务失败（签名错、关键词不符等）同样返回HTTP 200，
    // 只看状态码会把失败当成功、退避重试形同虚设，告警会被静默丢失，
    // 因此必须解析响应体中的业务码（errcode/code/StatusCode）
    pub(crate) async fn try_post(
        client: &Client,
        url: &str,
        body: &serde_json::Value,
        channel: &str,
    ) -> Result<(), String> {
        let resp = client
            .post(url)
            .json(body)
            .send()
            .await
            .map_err(|e| format!("请求错误 {}", e))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(format!("{}返回状态码 {}", channel, status));
        }
        let text = resp
            .text()
            .await
            .map_err(|e| format!("{}响应体读取失败: {}", channel, e))?;
        match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(v) => match business_error(&v) {
                Some(err) => Err(format!("{}业务错误: {}", channel, err)),
                None => Ok(()),
            },
            // 响应体不是JSON（如网关返回纯文本）：无从校验业务码，按成功处理避免误重试
            Err(_) => Ok(()),
        }
    }
}

// 从webhook响应JSON中提取业务错误：
// 钉钉/企业微信用errcode，飞书新版用code，飞书旧版用StatusCode；非零即业务失败
// 找到第一个业务码字段即停止：0→成功，非0→附平台返回的msg便于直接定位问题
// 响应体不含任何业务码字段时视为成功（无从校验，避免对兼容网关误重试）
fn business_error(v: &serde_json::Value) -> Option<String> {
    for field in ["errcode", "code", "StatusCode"] {
        if let Some(code) = v.get(field).and_then(|c| c.as_i64()) {
            if code == 0 {
                return None;
            }
            let msg = ["errmsg", "msg", "StatusMessage"]
                .iter()
                .copied()
                .find_map(|k| v.get(k).and_then(|m| m.as_str()))
                .unwrap_or("无错误详情");
            return Some(format!("{}={}，{}", field, code, msg));
        }
    }
    None
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

// 钉钉加签算法：以 secret 为HMAC-SHA256密钥，对 "{毫秒时间戳}\n{secret}" 签名后base64
// （与飞书方向相反：飞书密钥和时间戳串一起做密钥，钉钉时间戳串是被签消息）
fn dingtalk_sign(timestamp_millis: i64, secret: &str) -> String {
    use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
    use base64::Engine as _;
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    let string_to_sign = format!("{}\n{}", timestamp_millis, secret);
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
        .expect("HMAC-SHA256 accepts keys of any length");
    mac.update(string_to_sign.as_bytes());
    BASE64_STANDARD.encode(mac.finalize().into_bytes())
}

#[cfg(test)]
mod tests {
    use super::{dingtalk_sign, feishu_sign, NotifyEngine, RETRY_DELAYS_SECS, retry_with_backoff};

    #[test]
    fn feishu_sign_matches_reference_vector() {
        // 参考向量由PowerShell HMACSHA256独立计算：key = "1712126400\ntest-secret"，消息为空
        assert_eq!(
            feishu_sign(1712126400, "test-secret"),
            "BbGyLWSeiDDaf5+1ZZHc+miQjKW0aT9Rp/4iOd3ju5I="
        );
    }

    #[test]
    fn dingtalk_sign_matches_reference_vector() {
        // 参考向量由PowerShell HMACSHA256独立计算：
        // key = "test-secret"，消息 = "1712126400000\ntest-secret"（钉钉用毫秒时间戳）
        assert_eq!(
            dingtalk_sign(1712126400000, "test-secret"),
            "a3Oj/xZUNmKv482dWMT5v1Nhr+PAlFKrLxjD39MXOXA="
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

    #[test]
    fn business_error_detects_nonzero_errcode() {
        // 钉钉/企微形态：HTTP 200 + errcode!=0（签名错/关键词不符的典型返回）
        let v = serde_json::json!({"errcode": 310000, "errmsg": "sign not match"});
        let err = super::business_error(&v).expect("errcode!=0应识别为业务错误");
        assert!(err.contains("310000"), "应包含业务码: {err}");
        assert!(err.contains("sign not match"), "应包含平台错误详情: {err}");
    }

    #[test]
    fn business_error_zero_errcode_is_success() {
        let v = serde_json::json!({"errcode": 0, "errmsg": "ok"});
        assert!(super::business_error(&v).is_none(), "errcode=0应视为成功");
    }

    #[test]
    fn business_error_detects_feishu_code_field() {
        // 飞书新版用code，错误详情在msg
        let v = serde_json::json!({"code": 19021, "msg": "Sign match fail"});
        let err = super::business_error(&v).expect("code!=0应识别为业务错误");
        assert!(
            err.contains("19021") && err.contains("Sign match fail"),
            "应包含业务码与详情: {err}"
        );
    }

    #[test]
    fn business_error_ignores_body_without_code_field() {
        // 无业务码字段的响应体无从校验，按成功处理
        assert!(super::business_error(&serde_json::json!({"ok": true})).is_none());
        assert!(super::business_error(&serde_json::json!({"message": "fine"})).is_none());
    }

    // 本地假webhook：返回给定的HTTP响应文本，供try_post端到端验证业务码判定
    fn spawn_fake_webhook(response: String) -> std::net::SocketAddr {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                use std::io::{Read, Write};
                // 必须先读请求再回响应：带着未读数据关闭socket会触发RST，
                // 抢在客户端读取响应之前把连接打断（Windows下尤其明显）
                let mut buf = [0u8; 8192];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });
        addr
    }

    fn canned_200(resp_body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            resp_body.len(),
            resp_body
        )
    }

    #[tokio::test]
    async fn try_post_fails_on_2xx_with_business_error() {
        use std::time::Duration;
        use reqwest::Client;
        // HTTP 200但errcode非零：旧实现会误判成功导致告警静默丢失，必须报错并触发重试
        let addr = spawn_fake_webhook(canned_200(r#"{"errcode":310000,"errmsg":"sign not match"}"#));
        let client = Client::builder().timeout(Duration::from_secs(2)).build().unwrap();
        let body = serde_json::json!({"msgtype": "text", "text": {"content": "t"}});
        let r = NotifyEngine::try_post(&client, &format!("http://{addr}/hook"), &body, "钉钉").await;
        let err = r.expect_err("200+errcode!=0应判为业务失败");
        assert!(err.contains("业务错误"), "应标注为业务错误: {err}");
        assert!(err.contains("310000"), "应包含业务码: {err}");
    }

    #[tokio::test]
    async fn try_post_succeeds_on_2xx_with_zero_errcode() {
        use std::time::Duration;
        use reqwest::Client;
        let addr = spawn_fake_webhook(canned_200(r#"{"errcode":0,"errmsg":"ok"}"#));
        let client = Client::builder().timeout(Duration::from_secs(2)).build().unwrap();
        let body = serde_json::json!({"msgtype": "text", "text": {"content": "t"}});
        let r = NotifyEngine::try_post(&client, &format!("http://{addr}/hook"), &body, "钉钉").await;
        assert!(r.is_ok(), "errcode=0应视为发送成功: {r:?}");
    }
}
