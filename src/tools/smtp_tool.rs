// EMAIL告警渠道的发信封装：基于lettre（Rust生态的SMTP事实标准库）
// 端口语义由lettre的传输器构造函数决定：
//   465  → relay()：隐式TLS（主流邮箱服务商）
//   587  → starttls_relay()：先明文再STARTTLS升级，服务器不支持则报错（不降级明文）
//   其他 → builder_dangerous_no_tls()：明文直连（仅建议本地relay场景，发送时日志WARN）
// 认证：password为空时跳过凭据（适配内部免认证relay），否则AUTH机制由lettre协商

use crate::tools_types::EmailNotifyConfig;
use lettre::{
    message::header::ContentType,
    message::Mailbox,
    transport::smtp::authentication::Credentials,
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
};

// 发送一封UTF-8文本邮件。返回Err时附带失败环节上下文，供通知重试日志直接定位
pub async fn send_mail(cfg: &EmailNotifyConfig, subject: &str, body: &str) -> Result<(), String> {
    if cfg.to.is_empty() {
        return Err("EMAIL通知缺少收件人（email.to为空）".to_string());
    }
    let from: Mailbox = cfg
        .from
        .clone()
        .unwrap_or_else(|| cfg.username.clone())
        .parse()
        .map_err(|e| format!("发件人地址非法: {e}"))?;
    let mut builder = Message::builder()
        .from(from)
        // message_id显式关闭：lettre自动生成依赖机器hostname，容器内可能取不到
        .message_id(None);
    for rcpt in &cfg.to {
        let mailbox: Mailbox = rcpt
            .parse()
            .map_err(|e| format!("收件人地址非法 {rcpt}: {e}"))?;
        builder = builder.to(mailbox);
    }
    let email = builder
        .subject(subject)
        .header(ContentType::TEXT_PLAIN)
        .body(body.to_string())
        .map_err(|e| format!("构建邮件失败: {e}"))?;

    let host = cfg.smtp_host.as_str();
    let builder = match cfg.smtp_port {
        465 => AsyncSmtpTransport::<Tokio1Executor>::relay(host)
            .map_err(|e| format!("创建SMTP传输器失败: {e}"))?,
        587 => AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(host)
            .map_err(|e| format!("创建STARTTLS传输器失败: {e}"))?,
        p => {
            tracing::warn!("SMTP端口 {p} 使用明文连接（凭证可能被窃听），建议465/587");
            AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(host)
        }
    }
    .port(cfg.smtp_port);
    let builder = if cfg.password.is_empty() {
        builder
    } else {
        builder.credentials(Credentials::new(
            cfg.username.clone(),
            cfg.password.clone(),
        ))
    };
    let transporter: AsyncSmtpTransport<Tokio1Executor> = builder.build();
    transporter
        .send(email)
        .await
        .map_err(|e| format!("SMTP发送失败: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::send_mail;
    use crate::tools_types::EmailNotifyConfig;

    // 本地假SMTP服务器：按脚本逐行回响应，收集收到的命令行供断言
    fn spawn_fake_smtp() -> (std::net::SocketAddr, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let transcript = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let t = transcript.clone();
        std::thread::spawn(move || {
            use std::io::{BufRead, BufReader, Write};
            if let Ok((mut stream, _)) = listener.accept() {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                stream.write_all(b"220 fake-smtp ready\r\n").unwrap();
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        break;
                    }
                    let cmd = line.trim_end().to_string();
                    // DATA载荷（含头部+正文）到单独的"."结束；期间原样收集不回响应
                    if cmd == "DATA" {
                        stream.write_all(b"354 go\r\n").unwrap();
                        loop {
                            let mut body_line = String::new();
                            if reader.read_line(&mut body_line).unwrap_or(0) == 0 {
                                return;
                            }
                            let done = body_line.trim_end() == ".";
                            t.lock().unwrap().push(body_line.trim_end().to_string());
                            if done {
                                break;
                            }
                        }
                        stream.write_all(b"250 queued\r\n").unwrap();
                        continue;
                    }
                    t.lock().unwrap().push(cmd.clone());
                    let resp = if cmd.starts_with("EHLO") {
                        // 必须宣告AUTH机制：lettre为安全只对宣告了AUTH的服务器发凭证，
                        // 未宣告时报"No compatible authentication mechanism"
                        "250-localhost\r\n250-AUTH PLAIN\r\n250 SIZE 10485760\r\n"
                    } else if cmd.starts_with("AUTH") {
                        "235 ok\r\n"
                    } else if cmd.starts_with("MAIL") || cmd.starts_with("RCPT") {
                        "250 ok\r\n"
                    } else if cmd.starts_with("QUIT") {
                        stream.write_all(b"221 bye\r\n").unwrap();
                        break;
                    } else {
                        "250 ok\r\n"
                    };
                    stream.write_all(resp.as_bytes()).unwrap();
                }
            }
        });
        (addr, transcript)
    }

    fn email_cfg(port: u16) -> EmailNotifyConfig {
        EmailNotifyConfig {
            smtp_host: "127.0.0.1".to_string(),
            smtp_port: port,
            username: "monitor@example.com".to_string(),
            password: "auth-code".to_string(),
            from: Some("monitor@example.com".to_string()),
            to: vec!["ops@example.com".to_string(), "boss@example.com".to_string()],
        }
    }

    // 端到端：完整SMTP会话（EHLO→AUTH→MAIL→RCPT×2→DATA→QUIT），信封与正文落位正确
    #[tokio::test]
    async fn send_mail_completes_full_smtp_session() {
        let (addr, transcript) = spawn_fake_smtp();
        send_mail(&email_cfg(addr.port()), "监控告警", "CPU usage 95.0%")
            .await
            .expect("假SMTP会话应完整走通");
        let t = transcript.lock().unwrap().join("\n");
        assert!(t.contains("EHLO"), "应有EHLO: {t}");
        assert!(t.contains("AUTH PLAIN "), "password非空应认证: {t}");
        assert!(
            t.contains("MAIL FROM:<monitor@example.com>"),
            "应有MAIL FROM: {t}"
        );
        assert!(
            t.contains("RCPT TO:<ops@example.com>") && t.contains("RCPT TO:<boss@example.com>"),
            "多收件人逐个RCPT: {t}"
        );
        assert!(t.contains("Subject: "), "应有主题头: {t}");
        assert!(
            t.contains("CPU usage 95.0%"),
            "ASCII正文应可还原: {t}"
        );
    }

    // 免认证relay场景：password为空不应出现AUTH命令
    #[tokio::test]
    async fn send_mail_skips_auth_when_password_empty() {
        let (addr, transcript) = spawn_fake_smtp();
        let mut cfg = email_cfg(addr.port());
        cfg.password = String::new();
        send_mail(&cfg, "t", "b").await.expect("免认证应走通");
        let t = transcript.lock().unwrap().join("\n");
        assert!(!t.contains("AUTH"), "password为空不应认证: {t}");
    }

    // 收件人列表为空：构造期即报错，不发起SMTP会话
    #[tokio::test]
    async fn send_mail_rejects_empty_recipients() {
        let (addr, _transcript) = spawn_fake_smtp();
        let mut cfg = email_cfg(addr.port());
        cfg.to = vec![];
        let err = send_mail(&cfg, "t", "b").await.expect_err("空收件人应报错");
        assert!(err.contains("收件人"), "错误应指明收件人缺失: {err}");
    }
}
