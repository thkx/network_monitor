DROP INDEX IF EXISTS idx_check_result_created_at;
DROP INDEX IF EXISTS idx_check_result_monitor_created;
CREATE INDEX idx_monitor_config_enabled ON monitor_config(enabled);
