-- check_result 时间维度索引：
-- 1) idx_check_result_created_at —— 保留策略按天清理（WHERE created_at < 截止）此前全表扫描
-- 2) idx_check_result_monitor_created —— 按监控查询结果（/api/results?monitor_id=）的时间序列读取
CREATE INDEX IF NOT EXISTS idx_check_result_created_at ON check_result(created_at);
CREATE INDEX IF NOT EXISTS idx_check_result_monitor_created ON check_result(monitor_id, created_at);

-- 冗余索引清理：idx_monitor_config_enabled 是 (enabled, id DESC) 的最左前缀，二者留一即可
DROP INDEX IF EXISTS idx_monitor_config_enabled;
