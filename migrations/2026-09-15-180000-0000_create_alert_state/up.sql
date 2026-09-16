-- 告警抑制状态表：持久化每个监控的"已告警未恢复"标志
-- 重启后引擎从本表恢复状态，避免"故障还在却重复告警"或"误报恢复"
CREATE TABLE alert_state (
    monitor_id INTEGER PRIMARY KEY REFERENCES monitor_config(id) ON DELETE CASCADE,
    alerting INTEGER NOT NULL DEFAULT 0,
    updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

CREATE TRIGGER trg_alert_state_updated_at
AFTER UPDATE ON alert_state
WHEN NEW.updated_at = OLD.updated_at
BEGIN
  UPDATE alert_state SET updated_at = CURRENT_TIMESTAMP WHERE monitor_id = OLD.monitor_id;
END;
