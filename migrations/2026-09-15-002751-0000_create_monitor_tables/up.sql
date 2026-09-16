-- Your SQL goes here
--监控配置表
CREATE TABLE monitor_config (
    id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
    name TEXT UNIQUE,
    target TEXT NOT NULL,
    method TEXT,
    monitor_type TEXT NOT NULL CHECK (monitor_type IN ('HTTP', 'TCP', 'UDP', 'ICMP', 'DNS', 'FTP', 'TRACEROUTE', 'CPU', 'DISK', 'MEMORY', 'PROCESS', 'UNKNOWN')),
    interval_ms INTEGER,
    timeout_ms INTEGER NOT NULL DEFAULT 5000,
    config_json TEXT,
    enabled INTEGER NOT NULL DEFAULT 1,
    tag TEXT,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

-- 检查结果表
CREATE TABLE check_result (
    id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
    monitor_id INTEGER NOT NULL REFERENCES monitor_config(id) ON DELETE CASCADE,
    monitor_type TEXT NOT NULL,
    status INTEGER NOT NULL,
    response_time INTEGER NOT NULL,
    metadata_json TEXT,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);


-- 索引创建
CREATE INDEX idx_monitor_config_enabled ON monitor_config(enabled); -- SELECT * FROM monitor_config WHERE enabled = 1;
-- 启用/禁用筛选 + 按 id 逆序分页
CREATE INDEX IF NOT EXISTS idx_monitor_config_enabled_id ON monitor_config(enabled, id DESC); -- SELECT * FROM monitor_config WHERE enabled = 1 ORDER BY id DESC LIMIT 10 OFFSET 20;
-- 目标精确/前缀搜索
CREATE INDEX IF NOT EXISTS idx_monitor_config_target ON monitor_config(target); -- SELECT * FROM monitor_config WHERE target LIKE 'http%';
-- 标签过滤
CREATE INDEX IF NOT EXISTS idx_monitor_config_tag ON monitor_config(tag); -- SELECT * FROM monitor_config WHERE tag = 'production';
CREATE INDEX idx_check_result_monitor_id ON check_result(monitor_id); -- SELECT * FROM check_result WHERE monitor_id = 1;

-- 触发器创建
CREATE TRIGGER trg_monitor_config_updated_at
AFTER UPDATE ON monitor_config
WHEN NEW.updated_at = OLD.updated_at
BEGIN
  UPDATE monitor_config SET updated_at = CURRENT_TIMESTAMP WHERE id = OLD.id;
END;

CREATE TRIGGER trg_check_result_updated_at
AFTER UPDATE ON check_result
WHEN NEW.updated_at = OLD.updated_at
BEGIN
  UPDATE check_result SET updated_at = CURRENT_TIMESTAMP WHERE id = OLD.id;
END;

