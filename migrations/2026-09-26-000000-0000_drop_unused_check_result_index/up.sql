-- 删除读侧无用的复合索引 idx_check_result_monitor_created (monitor_id, created_at)。
-- result_repo 的所有查询都按 id（rowid 别名）降序排序与游标翻页，实际命中的是单列索引
-- idx_check_result_monitor_id：SQLite 单列索引隐式追加 rowid（即 id），故其等价于
-- (monitor_id, id)，天然服务 `WHERE monitor_id = ? AND id < ? ORDER BY id DESC` 的 keyset。
-- 复合索引在读侧零使用，仅给最热写表 check_result 增加每行写放大，故移除。
DROP INDEX IF EXISTS idx_check_result_monitor_created;
