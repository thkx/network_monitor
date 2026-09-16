use diesel::{
    connection::SimpleConnection,
    prelude::*,
    r2d2::{ConnectionManager, Pool, PooledConnection},
};
use diesel_migrations::{EmbeddedMigrations, MigrationHarness, embed_migrations};

use backon::Retryable;

use std::{env, error::Error, sync::Arc, time::Duration};

use crate::tools::default_retry_policy;

// 数据库链接方法
// 定义链接池返回的数据类型
pub type SqlitePool = Pool<ConnectionManager<SqliteConnection>>;
pub type SqlitePooledConnection = PooledConnection<ConnectionManager<SqliteConnection>>;
// 使用diesel_migrations 方便进行数据的迁移
pub const MIGRATIONS: EmbeddedMigrations = embed_migrations!(); // 默认 查找与cargo.toml同级目录下的migrations文件夹

pub fn establish_connection() -> Result<SqlitePool, Box<dyn Error>> {
    let database_url = env::var("DATABASE_URL").unwrap_or_else(|_| "./db/monitor.db".to_string());
    let manager = ConnectionManager::<SqliteConnection>::new(database_url);
    let pool = Pool::builder()
        .max_size(8) // 设置最大链接数
        .connection_timeout(Duration::from_secs(30)) // 设置超时时间
        .build(manager)
        .map_err(|e| format!("Failed to create DB pool: {}", e))?;
    let mut conn = pool
        .get()
        .map_err(|e| format!("Failed to get DB connection from pool: {}", e))?;
    // WAL模式持久化在库文件上，对所有后续连接生效：读写不再互斥，
    // 大幅降低Web API读与监控结果写并发时的 database is locked 概率
    conn.batch_execute("PRAGMA journal_mode = WAL;")
        .map_err(|e| format!("Failed to set WAL journal mode: {}", e))?;
    // 外键约束是连接级参数（无法持久化到库文件）：不开启则 ON DELETE CASCADE 全部失效，
    // 删监控会留下 check_result/alert_state 孤儿行
    conn.batch_execute("PRAGMA foreign_keys = ON;")
        .map_err(|e| format!("Failed to enable foreign keys: {}", e))?;
    conn.run_pending_migrations(MIGRATIONS)
        .map_err(|e| format!("Failed to run database migrations: {}", e))?;
    Ok(pool)
}

// 定义一个重试的方法
pub async fn establish_database_connection() -> Result<Arc<SqlitePool>, Box<dyn Error>> {
    // 引入 backon 库 来实现链接方法重试的逻辑  都是通用的逻辑
    let pool = (|| async { establish_connection() }) // 把需要重试的内容 封装成一个闭包函数
        .retry(default_retry_policy()) // 使用默认的重试函数
        .await
        .map(Arc::new) // 使用Arc进行封装 方便进行线程间共享
        .map_err(|e| {
            eprintln!("数据库连接重试失败: {}", e);
            e
        })?;
    Ok(pool)
}

// 定义获取链接方法 方便业务层获取统一的链接池
pub fn get_connection(pool: &SqlitePool) -> SqlitePooledConnection {
    let mut conn = pool.get().expect("Failed to get connection from pool.");
    // 连接级参数每次取出连接时设置：busy_timeout遇锁最多等5秒；
    // foreign_keys开启级联删除（删监控自动清理check_result/alert_state）
    let _ = conn.batch_execute("PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 5000;");
    conn
}

// 测试辅助：在临时目录建库（自动跑迁移），测试结束随临时目录一起删除
#[cfg(test)]
pub fn test_pool(dir: &std::path::Path) -> SqlitePool {
    let url = format!("file:///{}/test.db", dir.display().to_string().replace('\\', "/"));
    let manager = ConnectionManager::<SqliteConnection>::new(url);
    let pool = Pool::builder()
        .max_size(4)
        .connection_timeout(Duration::from_secs(10))
        .build(manager)
        .expect("测试连接池创建失败");
    let mut conn = pool.get().expect("测试连接获取失败");
    conn.run_pending_migrations(MIGRATIONS)
        .expect("测试迁移执行失败");
    pool
}

#[cfg(test)]
mod tests {
    use super::test_pool;
    use crate::database::schema::monitor_config;
    use diesel::prelude::*;

    #[test]
    fn pool_connects_and_migrations_apply() {
        let dir = tempfile::tempdir().expect("临时目录创建失败");
        let pool = test_pool(dir.path());
        let mut conn = super::get_connection(&pool);
        // 迁移执行后monitor_config表应存在且为空
        let count: i64 = monitor_config::table
            .count()
            .get_result(&mut conn)
            .expect("查询应成功");
        assert_eq!(count, 0);
    }
}
