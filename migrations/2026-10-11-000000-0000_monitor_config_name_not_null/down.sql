-- 回滚 ADR-0007 B13：移除 name 非空守卫触发器
DROP TRIGGER IF EXISTS trg_monitor_config_name_not_null_update;
DROP TRIGGER IF EXISTS trg_monitor_config_name_not_null_insert;
