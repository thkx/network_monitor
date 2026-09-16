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
    // busy_timeout是连接级参数（无法像WAL一样持久化到库文件），每次取出连接时设置：
    // 遇到锁时最多等待5秒再报错，而不是立刻抛 database is locked
    let _ = conn.batch_execute("PRAGMA busy_timeout = 5000;");
    conn
}
