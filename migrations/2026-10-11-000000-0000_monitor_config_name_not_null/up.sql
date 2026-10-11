-- ADR-0007 B13：monitor_config.name 是可空列上的 UNIQUE 约束。
-- SQLite 中多个 NULL 互不相等，故可插入多条 name 为 NULL 的记录，
-- 使"按名称去重（find_by_name / 导入幂等）"被静默绕过。
--
-- 直接把列改成 NOT NULL 需要整表重建；而在 diesel 的事务内迁移中 DROP 父表会触发
-- check_result 的 ON DELETE CASCADE 隐式删除（数据丢失），且事务内无法关闭 foreign_keys。
-- 因此改用additive的守卫触发器：拒绝写入 NULL 的 name（应用层本就恒以 target 填充 name），
-- 等效地堵住"无名记录"绕过去重的漏洞，且无整表重建、无数据丢失风险。
CREATE TRIGGER IF NOT EXISTS trg_monitor_config_name_not_null_insert
BEFORE INSERT ON monitor_config
WHEN NEW.name IS NULL
BEGIN
  SELECT RAISE(ABORT, 'monitor_config.name 不可为空（去重依赖名称，禁止无名记录）');
END;

CREATE TRIGGER IF NOT EXISTS trg_monitor_config_name_not_null_update
BEFORE UPDATE OF name ON monitor_config
WHEN NEW.name IS NULL
BEGIN
  SELECT RAISE(ABORT, 'monitor_config.name 不可被更新为空');
END;
