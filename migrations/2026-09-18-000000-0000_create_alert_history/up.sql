-- 告警历史表：记录每次"触发/恢复"事件，用于控制台回溯与审计
-- （alert_state 只存当前抑制状态，无法回答"何时告过警、告了几次"）
-- monitor_id 外键级联删除：监控配置删除时其历史一并清理
-- id 为 rowid 别名、随插入单调递增，配合 (monitor_id, id) 索引支持 keyset 分页
CREATE TABLE alert_history (
    id INTEGER PRIMARY KEY,
    monitor_id INTEGER NOT NULL REFERENCES monitor_config(id) ON DELETE CASCADE,
    alert_type TEXT NOT NULL,   -- 监控类型（HTTP/ICMP/CPU...），标识告警来源
    state TEXT NOT NULL,        -- 'triggered'（进入告警）| 'recovered'（异常恢复）
    message TEXT,               -- 通知渠道发出的文本
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX IF NOT EXISTS idx_alert_history_monitor_id ON alert_history(monitor_id, id);
