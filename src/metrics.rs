// Prometheus指标模块：内存指标注册表 + 文本exposition格式渲染
// 消费循环每收到一条结果调用 record 更新；/metrics 端点 scrape 时调用 render
// 指标为内存态：进程重启后计数归零（Prometheus 对 counter 重置有 rate() 兼容，
// 历史数据仍以 check_result 表为完整事实来源）

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::async_monitor::ResultRoute;

// 单个监控的指标状态
#[derive(Debug, Default, Clone)]
struct MonitorMetrics {
    checks_ok_total: u64,       // 检查成功累计次数
    checks_failed_total: u64,   // 检查失败累计次数
    last_response_time_ms: u64, // 最近一次检查耗时
    last_check_ts: u64,         // 最近一次检查的unix秒（0=尚未检查）
    last_up: u8,                // 最近一次结果：1可用/0不可用
}

// 展示元信息：随每次结果刷新（配置改名/换类型后自动跟随）
#[derive(Debug, Clone)]
struct Meta {
    name: String,
    monitor_type: String,
}

// 指标注册表：monitor_id -> (metrics, meta)，Mutex保护跨任务读写
pub struct MetricsRegistry {
    inner: Mutex<HashMap<i32, (MonitorMetrics, Meta)>>,
}

impl MetricsRegistry {
    pub fn new() -> Self {
        MetricsRegistry {
            inner: Mutex::new(HashMap::new()),
        }
    }

    // 消费循环更新：成功/失败计数、最近耗时、最近检查时间
    // monitor_id为None（Once/Monitor模式）不记录——/metrics 仅Server模式暴露
    pub fn record(
        &self,
        route: &ResultRoute,
        monitor_type: &str,
        status: bool,
        response_time_ms: u64,
    ) {
        let Some(id) = route.monitor_id else {
            return;
        };
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let mut map = self.inner.lock().expect("metrics锁中毒");
        let entry = map.entry(id).or_insert_with(|| {
            (
                MonitorMetrics::default(),
                Meta {
                    name: route.name.clone(),
                    monitor_type: monitor_type.to_string(),
                },
            )
        });
        // 元信息随每次结果刷新，配置热更新改名后标签自动跟随
        entry.1 = Meta {
            name: route.name.clone(),
            monitor_type: monitor_type.to_string(),
        };
        if status {
            entry.0.checks_ok_total += 1;
        } else {
            entry.0.checks_failed_total += 1;
        }
        entry.0.last_response_time_ms = response_time_ms;
        entry.0.last_check_ts = ts;
        entry.0.last_up = u8::from(status);
    }

    // 渲染 Prometheus 文本格式（exposition version 0.0.4）
    // alerting：alert_state表的当前抑制状态快照；None时省略 monitor_alerting 家族
    pub fn render(&self, alerting: Option<&[(i32, bool)]>) -> String {
        let map = self.inner.lock().expect("metrics锁中毒");
        let alerting_map: HashMap<i32, bool> = alerting
            .map(|v| v.iter().copied().collect())
            .unwrap_or_default();
        // 按monitor_id排序输出，保证抓取间样本顺序稳定
        let mut ids: Vec<i32> = map.keys().copied().collect();
        ids.sort_unstable();

        let mut out = String::new();
        out.push_str(
            "# HELP network_monitor_up 目标最近一次检查是否可用（1可用/0不可用）\n",
        );
        out.push_str("# TYPE network_monitor_up gauge\n");
        out.push_str("# HELP network_monitor_checks_total 检查总次数（按结果状态分组）\n");
        out.push_str("# TYPE network_monitor_checks_total counter\n");
        out.push_str(
            "# HELP network_monitor_last_response_time_milliseconds 最近一次检查耗时（毫秒）\n",
        );
        out.push_str("# TYPE network_monitor_last_response_time_milliseconds gauge\n");
        out.push_str(
            "# HELP network_monitor_last_check_timestamp_seconds 最近一次检查时间（unix秒）\n",
        );
        out.push_str("# TYPE network_monitor_last_check_timestamp_seconds gauge\n");
        if alerting.is_some() {
            out.push_str(
                "# HELP network_monitor_alerting 是否处于告警抑制状态（1已告警未恢复）\n",
            );
            out.push_str("# TYPE network_monitor_alerting gauge\n");
        }

        for id in ids {
            let Some((m, meta)) = map.get(&id) else {
                continue;
            };
            let labels = format!(
                "monitor_id=\"{}\",name=\"{}\",type=\"{}\"",
                id,
                escape_label(&meta.name),
                escape_label(&meta.monitor_type)
            );
            out.push_str(&format!(
                "network_monitor_up{{{}}} {}\n",
                labels, m.last_up
            ));
            out.push_str(&format!(
                "network_monitor_checks_total{{{labels},status=\"ok\"}} {}\n",
                m.checks_ok_total
            ));
            out.push_str(&format!(
                "network_monitor_checks_total{{{labels},status=\"failed\"}} {}\n",
                m.checks_failed_total
            ));
            out.push_str(&format!(
                "network_monitor_last_response_time_milliseconds{{{labels}}} {}\n",
                m.last_response_time_ms
            ));
            out.push_str(&format!(
                "network_monitor_last_check_timestamp_seconds{{{labels}}} {}\n",
                m.last_check_ts
            ));
            if alerting.is_some() {
                let alerting_v = i32::from(alerting_map.get(&id).copied().unwrap_or(false));
                out.push_str(&format!(
                    "network_monitor_alerting{{{labels}}} {}\n",
                    alerting_v
                ));
            }
        }
        out
    }
}

impl Default for MetricsRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// Prometheus标签值转义：反斜杠、双引号、换行
fn escape_label(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

#[cfg(test)]
mod tests {
    use super::{escape_label, MetricsRegistry};
    use crate::async_monitor::ResultRoute;

    fn route(id: Option<i32>, name: &str) -> ResultRoute {
        ResultRoute {
            name: name.to_string(),
            monitor_id: id,
        }
    }

    #[test]
    fn render_contains_help_type_and_samples() {
        let registry = MetricsRegistry::new();
        registry.record(&route(Some(7), "https://baidu.com"), "HTTP", true, 120);
        registry.record(&route(Some(7), "https://baidu.com"), "HTTP", false, 0);
        let out = registry.render(None);
        assert!(out.contains("# TYPE network_monitor_up gauge"));
        assert!(out.contains("# TYPE network_monitor_checks_total counter"));
        assert!(out.contains(
            "network_monitor_checks_total{monitor_id=\"7\",name=\"https://baidu.com\",type=\"HTTP\",status=\"ok\"} 1"
        ));
        assert!(out.contains("status=\"failed\"} 1"));
        assert!(out.contains("network_monitor_last_response_time_milliseconds{monitor_id=\"7\""));
        assert!(out.contains("network_monitor_last_check_timestamp_seconds{monitor_id=\"7\""));
        assert!(!out.contains("network_monitor_alerting"), "None时不输出alerting家族");
    }

    #[test]
    fn label_values_are_escaped() {
        let registry = MetricsRegistry::new();
        registry.record(
            &route(Some(1), "https://a.com/\"x\"\\y"),
            "HTTP",
            true,
            1,
        );
        let out = registry.render(None);
        // 引号与反斜杠必须转义，避免破坏Prometheus文本解析
        assert!(out.contains("name=\"https://a.com/\\\"x\\\"\\\\y\""));
    }

    #[test]
    fn multiple_monitors_are_isolated() {
        let registry = MetricsRegistry::new();
        registry.record(&route(Some(1), "a"), "HTTP", true, 10);
        registry.record(&route(Some(2), "b"), "ICMP", false, 20);
        registry.record(&route(Some(2), "b"), "ICMP", false, 30);
        let out = registry.render(None);
        assert!(out.contains("network_monitor_up{monitor_id=\"1\",name=\"a\",type=\"HTTP\"} 1"));
        assert!(out.contains("network_monitor_up{monitor_id=\"2\",name=\"b\",type=\"ICMP\"} 0"));
        // 样本按monitor_id升序：id=1的行出现在id=2之前
        let pos1 = out.find("monitor_id=\"1\"").unwrap();
        let pos2 = out.find("monitor_id=\"2\"").unwrap();
        assert!(pos1 < pos2);
    }

    #[test]
    fn missing_monitor_id_is_skipped() {
        let registry = MetricsRegistry::new();
        registry.record(&route(None, "once-target"), "HTTP", true, 5);
        let out = registry.render(None);
        // 无monitor_id的结果不产生样本行，仅保留合法的HELP/TYPE头
        assert!(!out.contains("network_monitor_up{"));
        assert!(!out.contains("status=\"ok\""));
    }

    #[test]
    fn alerting_merge_from_snapshot() {
        let registry = MetricsRegistry::new();
        registry.record(&route(Some(1), "a"), "HTTP", true, 5);
        registry.record(&route(Some(2), "b"), "HTTP", true, 5);
        let out = registry.render(Some(&[(1, true)]));
        assert!(out.contains("network_monitor_alerting{monitor_id=\"1\""));
        assert!(out.contains("# TYPE network_monitor_alerting gauge"));
        // id=1告警中=1，id=2无记录=0
        let line1 = out.lines().find(|l| l.contains("network_monitor_alerting") && l.contains("monitor_id=\"1\"")).unwrap();
        assert!(line1.ends_with(" 1"));
        let line2 = out.lines().find(|l| l.contains("network_monitor_alerting") && l.contains("monitor_id=\"2\"")).unwrap();
        assert!(line2.ends_with(" 0"));
    }

    #[test]
    fn escape_label_handles_specials() {
        assert_eq!(escape_label("a\\b\"c\nd"), "a\\\\b\\\"c\\nd");
    }
}
