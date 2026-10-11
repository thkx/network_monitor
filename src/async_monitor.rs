use crate::alerts::AlertsEngine;
use crate::domain::{CheckResult, MonitorConfig};
use crate::metrics::{ALERT_EVAL_ERRORS_TOTAL, RESULTS_DROPPED_TOTAL, TASK_PANICS_TOTAL};
use crate::monitor::Monitor;

use tokio::time::{Duration, interval};
// 引入mpsc 通道概念 用于创建通道 并将接收端返回给调用者
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

// A2 任务监督参数（ADR-0008）：
// 连续 panic 达上限即熔断停止该任务（避免持续 panic 刷日志/吃 CPU）；
// 每次 panic 后按指数退避重试，退避上限封顶，规避重启风暴（R1）
const MAX_CONSECUTIVE_PANICS: u32 = 5;
const PANIC_BACKOFF_CAP_SECS: u64 = 60;

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
    //
    // ADR-0008 运行时可靠性：
    //   A1 结果发送改 try_send——通道满则丢弃并计数、不阻塞探测（告警已在发送前完成，丢弃仅影响落库/CSV/指标）
    //   A2 每轮 check 放子任务执行，用 JoinError 捕获 panic：退避重试 + 连续超限熔断，不静默停产、不重启风暴
    //   A4 协作式关停：stop 置位后在循环顶干净退出（调度器 shutdown 置位 + 宽限期 + 兜底 abort）
    //   G3 告警评估 Err 计数（可观测），根治留待 Batch 4
    pub fn create_interval_monitoring(
        target: Box<dyn Monitor>,
        config: MonitorConfig,
        route: ResultRoute,
        engine: Option<AlertsEngine>,
        tx: mpsc::Sender<MonitorResultMessage>,
        stop: Arc<AtomicBool>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut engine = engine;
            // Arc 化以便每轮 clone 进捕获 panic 的子任务（check 为 &self，共享安全）
            let target: Arc<dyn Monitor> = Arc::from(target);
            let config = Arc::new(config);
            let mut timer = interval(Duration::from_secs(config.interval.unwrap_or(60)));
            // 若某轮 check 耗时超过 interval（如慢速探测），错过的 tick 不补偿、直接跳到下一个对齐点：
            // 避免积压的 tick 在恢复后连续密集触发（突发打满），对齐"按节奏采样"的监控语义
            timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut consecutive_panics: u32 = 0;
            loop {
                // A4：协作式关停——置位后在下一个循环顶干净退出
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                timer.tick().await;
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                // A2：check 放子任务执行，用 JoinError 捕获其 panic
                let t = target.clone();
                let cfg = config.clone();
                let check = tokio::spawn(async move { t.check(&cfg).await });
                let (status, details) = match check.await {
                    Ok(pair) => {
                        consecutive_panics = 0;
                        pair
                    }
                    Err(join_err) if join_err.is_cancelled() => {
                        // 子任务被取消（关停兜底 abort 波及）：正常退出
                        break;
                    }
                    Err(join_err) => {
                        consecutive_panics += 1;
                        TASK_PANICS_TOTAL.fetch_add(1, Ordering::Relaxed);
                        tracing::error!(
                            "监控 {} 的检查执行 panic（连续第 {} 次）: {}",
                            route.name,
                            consecutive_panics,
                            join_err
                        );
                        if consecutive_panics >= MAX_CONSECUTIVE_PANICS {
                            tracing::error!(
                                "监控 {} 连续 {} 次 panic，熔断停止该任务（修复后需重载配置重建任务）",
                                route.name,
                                MAX_CONSECUTIVE_PANICS
                            );
                            break;
                        }
                        // R1：指数退避（1s,2s,4s,8s…）后重试，退避上限封顶，避免重启风暴
                        let shift = (consecutive_panics - 1).min(6);
                        let backoff = (1u64 << shift).min(PANIC_BACKOFF_CAP_SECS);
                        tokio::time::sleep(Duration::from_secs(backoff)).await;
                        continue;
                    }
                };
                // 结果信封统一由包装层组装：id新生成、target来自config、
                // 类型来自引擎自述get_type（类型唯一事实来源，避免引擎散落硬编码）
                let check_result = CheckResult {
                    id: uuid::Uuid::new_v4().as_u128(),
                    monitor_type: target.get_type(),
                    target: config.target.clone(),
                    status,
                    details,
                };
                // 告警检查在任务内部完成，命中规则时发送通知（G3：Err 计数后仍记日志）
                if let Some(engine) = engine.as_mut()
                    && let Err(e) = engine.check(&check_result).await
                {
                    ALERT_EVAL_ERRORS_TOTAL.fetch_add(1, Ordering::Relaxed);
                    tracing::error!("告警检查失败: {}", e);
                }
                let message = MonitorResultMessage {
                    route: route.clone(),
                    result: check_result,
                };
                // A1：try_send 不阻塞探测——通道满时丢弃本条并计数（不影响已完成的告警），通道关则退出
                match tx.try_send(message) {
                    Ok(()) => {}
                    Err(TrySendError::Full(_)) => {
                        let n = RESULTS_DROPPED_TOTAL.fetch_add(1, Ordering::Relaxed) + 1;
                        // 限频：每 100 条丢弃记一次 warn，避免刷屏（累计数始终经 /metrics 可见）
                        if n % 100 == 1 {
                            tracing::warn!(
                                "结果通道已满，丢弃 {} 的检查结果（累计丢弃 {} 条，不影响告警，仅落库/CSV/指标缺口）",
                                route.name,
                                n
                            );
                        }
                    }
                    Err(TrySendError::Closed(_)) => {
                        // 消费端已关闭，结束本监控任务
                        tracing::error!("结果通道发送失败（消费端已关闭），监控任务退出");
                        break;
                    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::MonitorDefinition;
    use crate::monitor::MonitorFactory;

    // A4 回归：置协作式停止标志后，监控任务应在下一循环顶干净退出（handle 完成）
    #[tokio::test]
    async fn cooperative_stop_terminates_task() {
        let entry: MonitorDefinition =
            serde_json::from_str(r#"{"monitor_type":"CPU","timeout":5}"#).unwrap();
        let config = MonitorConfig::from_entry(&entry, 1);
        let monitor = MonitorFactory::create_monitor(config.monitor_type);
        let (tx, mut rx) = mpsc::channel(100);
        let stop = Arc::new(AtomicBool::new(false));
        let handle = AsyncMonitor::create_interval_monitoring(
            monitor,
            config,
            ResultRoute {
                name: "t".to_string(),
                monitor_id: None,
            },
            None,
            tx,
            stop.clone(),
        );
        // 任务在跑：首个 tick 立即触发，应收到一条结果（证明 A2 子任务包装下循环正常产出）
        let first = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await;
        assert!(matches!(first, Ok(Some(_))), "任务应产出首条结果");
        // 置停止标志：任务应在下一循环顶退出
        stop.store(true, Ordering::Relaxed);
        let joined = tokio::time::timeout(Duration::from_secs(5), handle).await;
        assert!(joined.is_ok(), "置停止标志后监控任务应退出");
    }
}
