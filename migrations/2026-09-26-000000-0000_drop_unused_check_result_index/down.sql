-- 回滚：重建复合索引（与 2026-09-17-000000-0000_add_check_result_indexes 建立时等价）
CREATE INDEX IF NOT EXISTS idx_check_result_monitor_created ON check_result(monitor_id, created_at);
