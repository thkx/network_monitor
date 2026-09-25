// 调度器模块：统一持有所有定时监控任务的句柄
// API 增删改/启停配置后调用 reload_all 整体重建（abort 旧任务 → 从数据库重新加载），实现热更新
// 配置量小，全量重建开销可忽略；后续需要时再演进为按 id 增量调度

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::alerts::AlertsEngine;
use crate::async_monitor::{AsyncMonitor, MonitorResultMessage, ResultRoute};
use crate::database::models::MonitorConfigModel;
use crate::database::pool::SqlitePool;
use crate::database::services::monitor_service::MonitorService;
use crate::domain::{MonitorDefinition, display_name};
use crate::monitor::MonitorFactory;
use crate::domain::MonitorConfig;

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

    // 优雅关停：停止全部监控任务并释放调度器持有的发送端。
    // 发送端全部drop后，结果消费者的recv返回None，触发最终攒批落库后退出
    pub fn shutdown(&mut self) {
        for (_id, handle) in self.tasks.drain() {
            handle.abort();
        }
        self.tx = None;
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
        let entry: MonitorDefinition = match serde_json::from_str(&config_json) {
            Ok(entry) => entry,
            Err(e) => {
                tracing::error!("监控配置 {} (id={}) 解析失败: {}", row.target, row.id, e);
                return;
            }
        };
        // 飞书webhook还是占位符时提前提示，避免告警真正触发时才发现发不出去
        if let Some(alert) = entry.alert_rules.as_ref()
            && alert
                .notify_config
                .webhook_url
                .contains(crate::domain::WEBHOOK_PLACEHOLDER)
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
            AlertsEngine::with_state(rules, self.pool.clone(), row.id)
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

    // 当前存活任务数（测试断言热更新/关停语义用）
    #[cfg(test)]
    pub(crate) fn active_task_count(&self) -> usize {
        self.tasks.len()
    }
}

#[cfg(test)]
mod tests {
    use super::Scheduler;
    use crate::database::models::{MonitorConfigInsert, MonitorConfigUpdate};
    use crate::database::pool::test_pool;
    use crate::database::repositories::monitor_repo::MonitorRepository;
    use crate::database::services::monitor_service::MonitorService;
    use std::sync::Arc;
    use tokio::sync::mpsc;

    // CPU类型监控：无网络依赖，任务可在测试内真实执行
    fn insert_cpu(pool: &Arc<crate::database::pool::SqlitePool>, name: &str) -> i32 {
        MonitorRepository::new(pool.clone())
            .create_monitor(&MonitorConfigInsert {
                name: Some(name.to_string()),
                target: "cpu://local".to_string(),
                method: None,
                monitor_type: "CPU".to_string(),
                interval_ms: Some(5),
                timeout_ms: 5000,
                config_json: Some(r#"{"monitor_type":"CPU","timeout":5}"#.to_string()),
                enabled: 1,
                tag: None,
            })
            .expect("测试监控创建失败")
            .id
    }

    fn set_enabled(pool: &Arc<crate::database::pool::SqlitePool>, id: i32, enabled: bool) {
        MonitorRepository::new(pool.clone())
            .update_monitor(
                id,
                &MonitorConfigUpdate {
                    enabled: Some(i32::from(enabled)),
                    name: None,
                    target: None,
                    method: None,
                    monitor_type: None,
                    interval_ms: None,
                    timeout_ms: None,
                    config_json: None,
                    tag: None,
                },
            )
            .expect("启停更新失败");
    }

    // 热更新语义端到端：加载→任务真实产出结果→禁用配置→重建后任务数收敛→关停归零
    #[tokio::test]
    async fn reload_runs_tasks_and_hot_update_applies() {
        let dir = tempfile::tempdir().expect("临时目录创建失败");
        let pool = Arc::new(test_pool(dir.path()));
        let service = MonitorService::new(MonitorRepository::new(pool.clone()));
        let id1 = insert_cpu(&pool, "t-sched-1");
        let _id2 = insert_cpu(&pool, "t-sched-2");

        let (tx, mut rx) = mpsc::channel(16);
        let mut scheduler = Scheduler::new(pool.clone(), 5);
        scheduler.set_sender(tx);
        scheduler.reload_all(&service);
        assert_eq!(
            scheduler.active_task_count(),
            2,
            "两个启用配置应各起一个任务"
        );

        // 任务真实运行：interval首tick立即触发，应收到带monitor_id的CPU检查结果
        let msg = tokio::time::timeout(std::time::Duration::from_secs(10), rx.recv())
            .await
            .expect("10秒内应产出结果")
            .expect("通道不应关闭");
        assert!(msg.route.monitor_id.is_some(), "Server模式结果应带主键");
        assert!(
            matches!(msg.result.monitor_type, crate::domain::MonitorType::Cpu),
            "应为CPU监控结果"
        );

        // 禁用一个配置后热更新：任务数收敛为1（此前"列表里消失"的回归场景）
        set_enabled(&pool, id1, false);
        scheduler.reload_all(&service);
        assert_eq!(scheduler.active_task_count(), 1, "禁用后应只剩1个任务");

        // 关停：任务清空且发送端释放（消费者的recv将最终返回None）
        scheduler.shutdown();
        assert_eq!(scheduler.active_task_count(), 0, "关停后任务应清空");
    }
}
