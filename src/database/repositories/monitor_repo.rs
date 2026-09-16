use crate::database::connect_db::{SqlitePool, get_connection};
use crate::database::models::{MonitorConfigInsert, MonitorConfigModel, MonitorConfigUpdate};
use crate::database::schema::monitor_config;
use diesel::prelude::*;
use diesel::result::Error;
use std::sync::Arc;

pub struct MonitorRepository {
    pool: Arc<SqlitePool>,
}

impl MonitorRepository {
    pub fn new(pool: Arc<SqlitePool>) -> Self {
        MonitorRepository { pool }
    }
    pub fn get_monitors_by_enabled(
        &self,
        enabled_flag: bool,
        page: i64,
        page_size: i64,
    ) -> Result<(Vec<MonitorConfigModel>, i64), Error> {
        // 获取当前对数据库的链接
        let mut conn = get_connection(&self.pool);
        let page_no = page.max(1);
        let page_sz = page_size.max(1);
        let offset = (page_no - 1) * page_sz;
        let enabled_v = if enabled_flag { 1 } else { 0 };

        let total: i64 = monitor_config::table
            .filter(monitor_config::enabled.eq(enabled_v))
            .count()
            .get_result(&mut conn)?;
        // 执行数据库操作
        let results = monitor_config::table
            .filter(monitor_config::enabled.eq(enabled_v))
            .order(monitor_config::id.desc())
            .limit(page_sz)
            .offset(offset)
            .load::<MonitorConfigModel>(&mut conn)?;
        Ok((results, total))
    }

    // 分页查询全部监控配置（不筛选启停状态）
    pub fn get_monitors_paged(
        &self,
        page: i64,
        page_size: i64,
    ) -> Result<(Vec<MonitorConfigModel>, i64), Error> {
        let mut conn = get_connection(&self.pool);
        let page_no = page.max(1);
        let page_sz = page_size.max(1);
        let offset = (page_no - 1) * page_sz;
        let total: i64 = monitor_config::table.count().get_result(&mut conn)?;
        let results = monitor_config::table
            .order(monitor_config::id.desc())
            .limit(page_sz)
            .offset(offset)
            .load::<MonitorConfigModel>(&mut conn)?;
        Ok((results, total))
    }

    // 查询所有启用的监控配置（不分页，供Server模式调度器加载）
    pub fn get_all_enabled(&self) -> Result<Vec<MonitorConfigModel>, Error> {
        let mut conn = get_connection(&self.pool);
        monitor_config::table
            .filter(monitor_config::enabled.eq(1))
            .order(monitor_config::id.asc())
            .load::<MonitorConfigModel>(&mut conn)
    }

    // 按ID查询监控配置
    pub fn get_monitor_by_id(&self, id: i32) -> Result<Option<MonitorConfigModel>, Error> {
        let mut conn = get_connection(&self.pool);
        monitor_config::table
            .find(id)
            .first::<MonitorConfigModel>(&mut conn)
            .optional()
    }

    // 按名称查询监控配置（name列有唯一约束，导入配置时用于去重）
    pub fn find_by_name(&self, name: &str) -> Result<Option<MonitorConfigModel>, Error> {
        let mut conn = get_connection(&self.pool);
        monitor_config::table
            .filter(monitor_config::name.eq(name))
            .first::<MonitorConfigModel>(&mut conn)
            .optional()
    }

    // 新增监控配置，返回插入后的完整记录
    pub fn create_monitor(
        &self,
        insert: &MonitorConfigInsert,
    ) -> Result<MonitorConfigModel, Error> {
        let mut conn = get_connection(&self.pool);
        diesel::insert_into(monitor_config::table)
            .values(insert)
            .returning(monitor_config::all_columns)
            .get_result(&mut conn)
    }

    // 按ID更新监控配置，返回受影响的行数
    pub fn update_monitor(&self, id: i32, update: &MonitorConfigUpdate) -> Result<usize, Error> {
        let mut conn = get_connection(&self.pool);
        diesel::update(monitor_config::table.find(id))
            .set(update)
            .execute(&mut conn)
    }

    // 按ID删除监控配置，返回受影响的行数（关联的check_result由外键级联删除）
    pub fn delete_monitor(&self, id: i32) -> Result<usize, Error> {
        let mut conn = get_connection(&self.pool);
        diesel::delete(monitor_config::table.find(id)).execute(&mut conn)
    }
}
