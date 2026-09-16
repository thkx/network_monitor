// 调度器模块：统一持有所有定时监控任务的句柄
// API 增删改/启停配置后调用 reload_all 整体重建（abort 旧任务 → 从数据库重新加载），实现热更新
// 配置量小，全量重建开销可忽略；后续需要时再演进为按 id 增量调度

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::alerts::AlertsEngine;
use crate::async_monitor::{AsyncMonitor, MonitorResultMessage, ResultRoute};
use crate::database::connect_db::SqlitePool;
use crate::database::models::MonitorConfigModel;
use crate::database::repositories::alert_state_repo::AlertStateRepository;
use crate::database::services::monitor_service::MonitorService;
use crate::monitor::MonitorFactory;
use crate::monitor::types::MonitorConfig;
use crate::tools_types::{SelfDefineMonitorConfig, display_name};

pub struct Scheduler {
    tasks: HashMap<i32, JoinHandle<()>>,
    tx: Option<mpsc::Sender<MonitorResultMessage>>,
    // 连接池：用于构造告警状态仓库（抑制状态持久化到 alert_state 表）
    pool: Arc<SqlitePool>,
    // 默认监控间隔（秒）：配置项未指定interval时使用（--interval，与Once/Monitor模式同源）
    default_interval: u64,
}

impl Scheduler {
    pub fn new(pool: Arc<SqlitePool>, default_interval: u64) -> Self {
        Scheduler {
            tasks: HashMap::new(),
            tx: None,
            pool,
            default_interval,
        }
    }

    // 绑定结果通道（在启动消费者之后调用一次）
    // 调度器持有发送端，通道在整个进程生命周期内保持打开，热更新不会导致消费者退出
    pub fn set_sender(&mut self, tx: mpsc::Sender<MonitorResultMessage>) {
        self.tx = Some(tx);
    }

    // 全量重建所有监控任务：先停止旧任务，再从数据库加载启用中的配置重新调度
    pub fn reload_all(&mut self, service: &MonitorService) {
        for (_id, handle) in self.tasks.drain() {
            handle.abort();
        }
        // clone一份sender，避免持有self的不可变借用阻塞后续spawn_one的可变借用
        let Some(tx) = self.tx.clone() else {
            return;
        };
        let rows = match service.get_all_enabled() {
            Ok(rows) => rows,
            Err(e) => {
                tracing::error!("调度器加载监控配置失败: {}", e);
                return;
            }
        };
        for row in rows {
            self.spawn_one(row, tx.clone());
        }
        tracing::info!("调度器已启动 {} 个定时监控任务", self.tasks.len());
    }

    // 停止单个监控任务（预留按 id 停止的能力，当前热更新统一走 reload_all）
    #[allow(dead_code)]
    pub fn stop_one(&mut self, id: i32) {
        if let Some(handle) = self.tasks.remove(&id) {
            handle.abort();
        }
    }

    // 把一条数据库配置变成常驻监控任务
    fn spawn_one(&mut self, row: MonitorConfigModel, tx: mpsc::Sender<MonitorResultMessage>) {
        // config_json保存了完整的原始配置，反序列化后复用统一的构建逻辑
        let Some(config_json) = row.config_json.clone() else {
            tracing::error!(
                "监控配置 {} (id={}) 缺少config_json，跳过",
                row.target,
                row.id
            );
            return;
        };
        let entry: SelfDefineMonitorConfig = match serde_json::from_str(&config_json) {
            Ok(entry) => entry,
            Err(e) => {
                tracing::error!("监控配置 {} (id={}) 解析失败: {}", row.target, row.id, e);
                return;
            }
        };
        // 飞书webhook还是占位符时提前提示，避免告警真正触发时才发现发不出去
        if let Some(alert) = entry.alert_rules.as_ref()
            && alert.notify_config.webhook_url.contains("you/to/path")
        {
            tracing::warn!(
                "监控 {} (id={}) 的 webhook_url 仍是占位符，告警通知将发送失败",
                row.target,
                row.id
            );
        }
        let name = row.name.clone().unwrap_or_else(|| display_name(&entry));
        let config = MonitorConfig::from_entry(&entry, self.default_interval);
        let monitor = MonitorFactory::create_monitor(config.monitor_type);
        // 告警引擎与任务同生命周期；抑制状态从 alert_state 表恢复：
        // 热更新重建任务后引擎不会"失忆"，故障未恢复就不会重复轰炸通知渠道
        let engine = entry.alert_rules.map(|rules| {
            let repo = AlertStateRepository::new(self.pool.clone());
            AlertsEngine::with_state(rules, repo, row.id)
        });
        let handle = AsyncMonitor::create_interval_monitoring(
            monitor,
            config,
            ResultRoute {
                name,
                monitor_id: Some(row.id),
            },
            engine,
            tx,
        );
        self.tasks.insert(row.id, handle);
    }
}
