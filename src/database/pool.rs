use diesel::{
    connection::SimpleConnection,
    prelude::*,
    r2d2::{ConnectionManager, CustomizeConnection, Pool, PooledConnection},
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

// r2d2 建连回调：仅在"物理连接创建后"触发一次（on_acquire 不是每次 get 都跑），
// 是设置连接级 PRAGMA 的正确位置。此前在 get_connection 里每次借连接都重跑 PRAGMA、
// 且用 `let _ =` 吞掉错误；现集中到建连时一次性设置，并把失败作为取连接错误上报
// （返回 Err 并 tracing::error! 记录），不再静默吞错。
#[derive(Debug)]
struct ConnectionPragmas;

impl CustomizeConnection<SqliteConnection, diesel::r2d2::Error> for ConnectionPragmas {
    fn on_acquire(&self, conn: &mut SqliteConnection) -> Result<(), diesel::r2d2::Error> {
        // 连接级 PRAGMA（无法持久化到库文件，必须每条物理连接设置）：
        //   foreign_keys = ON       —— 不开则 ON DELETE CASCADE 失效，删监控会留孤儿行
        //   busy_timeout = 5000     —— 遇写锁最多等 5 秒再报 database is locked
        //   synchronous = NORMAL    —— WAL 下安全且显著降低 fsync 开销（用户已确认）
        //   wal_autocheckpoint=1000 —— 每累积约 1000 页自动 checkpoint，防 WAL 文件无限膨胀
        conn.batch_execute(
            "PRAGMA foreign_keys = ON; \
             PRAGMA busy_timeout = 5000; \
             PRAGMA synchronous = NORMAL; \
             PRAGMA wal_autocheckpoint = 1000;",
        )
        .map_err(|e| {
            tracing::error!("建连时设置连接级 PRAGMA 失败: {}", e);
            diesel::r2d2::Error::QueryError(e)
        })
    }
}

pub fn establish_connection() -> Result<SqlitePool, Box<dyn Error>> {
    let database_url = env::var("DATABASE_URL").unwrap_or_else(|_| "./db/monitor.db".to_string());
    let manager = ConnectionManager::<SqliteConnection>::new(database_url);
    let pool = Pool::builder()
        // SQLite 为单写者：写操作在库级串行化，连接池主要服务并发读；8 条足够覆盖
        // actix-web 读峰值，再大也无法突破单写者瓶颈，反而徒增打开的文件句柄与内存
        .max_size(8)
        // 保持至少 1 条空闲连接：避免低峰回落或冷启动后首个请求现建连（含 on_acquire
        // 的 PRAGMA 往返）带来的首字节延迟毛刺
        .min_idle(Some(1))
        .connection_timeout(Duration::from_secs(30)) // 设置超时时间
        // 建连回调：每条物理连接创建后设置连接级 PRAGMA（foreign_keys/busy_timeout/
        // synchronous/wal_autocheckpoint），失败即作为取连接错误上报
        .connection_customizer(Box::new(ConnectionPragmas))
        .build(manager)
        .map_err(|e| format!("Failed to create DB pool: {}", e))?;
    let mut conn = pool
        .get()
        .map_err(|e| format!("Failed to get DB connection from pool: {}", e))?;
    // WAL模式持久化在库文件上，对所有后续连接生效：读写不再互斥，
    // 大幅降低Web API读与监控结果写并发时的 database is locked 概率
    // （foreign_keys 等连接级 PRAGMA 已由 ConnectionPragmas 建连回调统一设置）
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
            tracing::error!("数据库连接重试失败: {}", e);
            e
        })?;
    Ok(pool)
}

// 定义获取链接方法 方便业务层获取统一的链接池。
// 连接池耗尽/超时（连接泄漏、并发峰值）返回 Err 而非 panic——此前 expect 会把
// "取连接失败"升级为进程级 panic（虽多被 spawn_blocking 圈住只毁单个 task，
// 但热更新等同步上下文会静默失败）。用 diesel 的 QueryBuilderError 作为错误逃逸口，
// 让全部仓库函数（均返回 diesel::result::Error）经 ? 统一走已有的错误降级路径
pub fn get_connection(pool: &SqlitePool) -> Result<SqlitePooledConnection, diesel::result::Error> {
    let conn = pool.get().map_err(|e| {
        diesel::result::Error::QueryBuilderError(
            format!("从连接池获取连接失败（池耗尽或超时）: {e}").into(),
        )
    })?;
    // 连接级 PRAGMA（foreign_keys/busy_timeout/synchronous/wal_autocheckpoint）已在
    // ConnectionPragmas 建连回调（on_acquire）中一次性设置，这里不再每次借连接重跑、
    // 也不再 `let _ =` 吞错——设置失败会在建连阶段作为取连接错误上报
    Ok(conn)
}

// 测试辅助：在临时目录建库（自动跑迁移），测试结束随临时目录一起删除
#[cfg(test)]
pub fn test_pool(dir: &std::path::Path) -> SqlitePool {
    let url = format!(
        "file:///{}/test.db",
        dir.display().to_string().replace('\\', "/")
    );
    let manager = ConnectionManager::<SqliteConnection>::new(url);
    let pool = Pool::builder()
        .max_size(4)
        .connection_timeout(Duration::from_secs(10))
        // 测试池同样装配建连回调，使测试走与生产一致的连接级 PRAGMA（含 foreign_keys）
        .connection_customizer(Box::new(ConnectionPragmas))
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
        let mut conn = super::get_connection(&pool).expect("测试取连接应成功");
        // 迁移执行后monitor_config表应存在且为空
        let count: i64 = monitor_config::table
            .count()
            .get_result(&mut conn)
            .expect("查询应成功");
        assert_eq!(count, 0);
    }

    #[test]
    fn exhausted_pool_returns_err_not_panic() {
        use diesel::SqliteConnection;
        use diesel::r2d2::{ConnectionManager, Pool};
        use std::time::Duration;
        // max_size=1 且把唯一连接握在手里：再取连接必然超时。
        // 断言返回 Err（而非旧实现的 expect panic），让调用方经 ? 走错误降级路径
        let dir = tempfile::tempdir().expect("临时目录创建失败");
        let url = format!(
            "file:///{}/x.db",
            dir.path().display().to_string().replace('\\', "/")
        );
        let manager = ConnectionManager::<SqliteConnection>::new(url);
        let pool = Pool::builder()
            .max_size(1)
            .connection_timeout(Duration::from_millis(300))
            .build(manager)
            .expect("连接池创建失败");
        // SqliteConnection 不实现 Debug，用 match 取代 expect_err/expect
        let _held = match super::get_connection(&pool) {
            Ok(c) => c,
            Err(e) => panic!("首个连接应成功，实际: {e:?}"),
        };
        match super::get_connection(&pool) {
            Ok(_) => panic!("连接池耗尽应返回Err"),
            Err(diesel::result::Error::QueryBuilderError(_)) => {}
            Err(other) => panic!("应为QueryBuilderError，实际: {other:?}"),
        }
    }
}
