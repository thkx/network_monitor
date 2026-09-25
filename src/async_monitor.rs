use crate::alerts::AlertsEngine;
use crate::domain::{CheckResult, MonitorConfig};
use crate::monitor::Monitor;

use tokio::time::{Duration, interval};
// 引入mpsc 通道概念 用于创建通道 并将接收端返回给调用者
use tokio::sync::mpsc;

use std::future::Future;
use std::pin::Pin;

// 这里统一返回异步监控的类型，方便后续进行统一的监控结果检测
type MonitorResultReceiver = mpsc::Receiver<CheckResult>;
type PinnedReceiverFuture = Pin<Box<dyn Future<Output = MonitorResultReceiver> + Send>>;

// 结果路由信息：消费端据此展示日志名称、关联数据库主键
// 告警检查已下沉到各监控任务内部，不再需要路由到消费者侧的告警引擎注册表
#[derive(Debug, Clone)]
pub struct ResultRoute {
    // 日志展示名
    pub name: String,
    // Server模式下对应的monitor_config表主键，Monitor/Once模式为None
    pub monitor_id: Option<i32>,
}

// 监控结果消息：共享通道中流转的完整信封（路由信息 + 检查结果）
pub struct MonitorResultMessage {
    pub route: ResultRoute,
    pub result: CheckResult,
}

pub struct AsyncMonitor {}

impl AsyncMonitor {
    // 创建一个轮询的监控器：结果统一发送到调用方提供的共享通道
    // 每个任务独占自己的告警引擎与抑制状态（异常只告警一次、恢复时通知），
    // 配置变更时由调度器 abort 重建任务，抑制状态随任务一起重置
    // 返回任务句柄，供调度器在热更新时停止任务
    pub fn create_interval_monitoring(
        target: Box<dyn Monitor>,
        config: MonitorConfig,
        route: ResultRoute,
        engine: Option<AlertsEngine>,
        tx: mpsc::Sender<MonitorResultMessage>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut engine = engine;
            let mut timer = interval(Duration::from_secs(config.interval.unwrap_or(60)));
            loop {
                timer.tick().await;
                let (status, details) = target.check(&config).await;
                // 结果信封统一由包装层组装：id新生成、target来自config、
                // 类型来自引擎自述get_type（类型唯一事实来源，避免引擎散落硬编码）
                let check_result = CheckResult {
                    id: uuid::Uuid::new_v4().as_u128(),
                    monitor_type: target.get_type(),
                    target: config.target.clone(),
                    status,
                    details,
                };
                // 告警检查在任务内部完成，命中规则时发送通知
                if let Some(engine) = engine.as_mut()
                    && let Err(e) = engine.check(&check_result).await
                {
                    tracing::error!("告警检查失败: {}", e);
                }
                let message = MonitorResultMessage {
                    route: route.clone(),
                    result: check_result,
                };
                if tx.send(message).await.is_err() {
                    // 消费端已关闭，结束本监控任务
                    tracing::error!("结果通道发送失败（消费端已关闭），监控任务退出");
                    break;
                }
            }
        })
    }
    // 创建一个单次监控
    pub fn create_once_monitoring(
        target: Box<dyn Monitor>,
        config: MonitorConfig,
    ) -> PinnedReceiverFuture {
        Box::pin(async move {
            let (tx, rx) = mpsc::channel(1);
            tokio::spawn(async move {
                let (status, details) = target.check(&config).await;
                // 结果信封统一由包装层组装：id新生成、target来自config、
                // 类型来自引擎自述get_type（类型唯一事实来源，避免引擎散落硬编码）
                let check_result = CheckResult {
                    id: uuid::Uuid::new_v4().as_u128(),
                    monitor_type: target.get_type(),
                    target: config.target.clone(),
                    status,
                    details,
                };
                if tx.send(check_result).await.is_err() {
                    tracing::error!("单次监控结果发送失败（接收端已关闭）");
                }
            });
            // 返回接收端 对象
            rx
        })
    }
}
