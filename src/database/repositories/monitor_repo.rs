use crate::database::models::{MonitorConfigInsert, MonitorConfigModel, MonitorConfigUpdate};
use crate::database::pool::{SqlitePool, get_connection};
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
    // 列表查询：可选按启停状态、可选按分组标签筛选，分页返回（列表 + 总数）。
    // 用 into_boxed() 动态拼接筛选，避免 enabled×tag 组合各写一遍。
    pub fn list_monitors(
        &self,
        enabled: Option<bool>,
        tag: Option<&str>,
        page: i64,
        page_size: i64,
    ) -> Result<(Vec<MonitorConfigModel>, i64), Error> {
        let mut conn = get_connection(&self.pool)?;
        let page_no = page.max(1);
        let page_sz = page_size.max(1);
        let offset = (page_no - 1) * page_sz;

        let mut count_q = monitor_config::table.into_boxed();
        let mut list_q = monitor_config::table.into_boxed();
        if let Some(flag) = enabled {
            let v = if flag { 1 } else { 0 };
            count_q = count_q.filter(monitor_config::enabled.eq(v));
            list_q = list_q.filter(monitor_config::enabled.eq(v));
        }
        if let Some(t) = tag {
            let t = t.to_string();
            count_q = count_q.filter(monitor_config::tag.eq(t.clone()));
            list_q = list_q.filter(monitor_config::tag.eq(t));
        }

        let total: i64 = count_q.count().get_result(&mut conn)?;
        let results = list_q
            .order(monitor_config::id.desc())
            .limit(page_sz)
            .offset(offset)
            .load::<MonitorConfigModel>(&mut conn)?;
        Ok((results, total))
    }

    // 查询库中出现过的全部非空分组标签（去重、升序），供列表页筛选下拉
    pub fn get_distinct_tags(&self) -> Result<Vec<String>, Error> {
        let mut conn = get_connection(&self.pool)?;
        monitor_config::table
            .filter(monitor_config::tag.is_not_null())
            .select(monitor_config::tag.assume_not_null())
            .distinct()
            .order(monitor_config::tag.asc())
            .load::<String>(&mut conn)
    }

    // 全量查询所有监控配置（/api/status 控制台聚合视图用）
    // 此前状态视图误用分页接口并硬编码1000上限，配置超限会静默截尾
    pub fn get_all(&self) -> Result<Vec<MonitorConfigModel>, Error> {
        let mut conn = get_connection(&self.pool)?;
        monitor_config::table
            .order(monitor_config::id.asc())
            .load::<MonitorConfigModel>(&mut conn)
    }

    // 查询所有启用的监控配置（不分页，供Server模式调度器加载）
    pub fn get_all_enabled(&self) -> Result<Vec<MonitorConfigModel>, Error> {
        let mut conn = get_connection(&self.pool)?;
        monitor_config::table
            .filter(monitor_config::enabled.eq(1))
            .order(monitor_config::id.asc())
            .load::<MonitorConfigModel>(&mut conn)
    }

    // 按ID查询监控配置
    pub fn get_monitor_by_id(&self, id: i32) -> Result<Option<MonitorConfigModel>, Error> {
        let mut conn = get_connection(&self.pool)?;
        monitor_config::table
            .find(id)
            .first::<MonitorConfigModel>(&mut conn)
            .optional()
    }

    // 按名称查询监控配置（name列有唯一约束，导入配置时用于去重）
    pub fn find_by_name(&self, name: &str) -> Result<Option<MonitorConfigModel>, Error> {
        let mut conn = get_connection(&self.pool)?;
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
        let mut conn = get_connection(&self.pool)?;
        diesel::insert_into(monitor_config::table)
            .values(insert)
            .returning(monitor_config::all_columns)
            .get_result(&mut conn)
    }

    // 按ID更新监控配置，返回受影响的行数
    pub fn update_monitor(&self, id: i32, update: &MonitorConfigUpdate) -> Result<usize, Error> {
        let mut conn = get_connection(&self.pool)?;
        diesel::update(monitor_config::table.find(id))
            .set(update)
            .execute(&mut conn)
    }

    // 按ID删除监控配置，返回受影响的行数（关联的check_result由外键级联删除）
    pub fn delete_monitor(&self, id: i32) -> Result<usize, Error> {
        let mut conn = get_connection(&self.pool)?;
        diesel::delete(monitor_config::table.find(id)).execute(&mut conn)
    }
}

#[cfg(test)]
mod tests {
    use super::{MonitorConfigInsert, MonitorConfigUpdate, MonitorRepository};
    use crate::database::pool::test_pool;
    use std::sync::Arc;

    fn insert(name: &str) -> MonitorConfigInsert {
        MonitorConfigInsert {
            name: Some(name.to_string()),
            target: "https://a.com".to_string(),
            method: Some("GET".to_string()),
            monitor_type: "HTTP".to_string(),
            interval_ms: Some(5000),
            timeout_ms: 5000,
            config_json: Some("{}".to_string()),
            enabled: 1,
            tag: None,
        }
    }

    #[test]
    fn monitor_crud_and_unique_name() {
        // tempdir守卫必须持有到测试结束：内联在表达式里guard会立即drop，
        // Linux当场删除库文件导致后续写入报readonly（Windows因文件被占用删不掉而侥幸通过）
        let dir = tempfile::tempdir().unwrap();
        let repo = MonitorRepository::new(Arc::new(test_pool(dir.path())));
        // 创建并回读
        let created = repo.create_monitor(&insert("t1")).unwrap();
        assert_eq!(created.name.as_deref(), Some("t1"));
        assert!(repo.find_by_name("t1").unwrap().is_some());
        // name唯一约束：重复插入报错
        assert!(repo.create_monitor(&insert("t1")).is_err());
        // 更新启停状态
        let update = MonitorConfigUpdate {
            enabled: Some(0),
            name: None,
            target: None,
            method: None,
            monitor_type: None,
            interval_ms: None,
            timeout_ms: None,
            config_json: None,
            tag: None,
        };
        assert_eq!(repo.update_monitor(created.id, &update).unwrap(), 1);
        let (list, total) = repo.list_monitors(Some(false), None, 1, 10).unwrap();
        assert_eq!(total, 1);
        assert_eq!(list[0].id, created.id);
        // 删除后不可再查到
        assert_eq!(repo.delete_monitor(created.id).unwrap(), 1);
        assert!(repo.get_monitor_by_id(created.id).unwrap().is_none());
    }

    // tag 筛选 + 去重标签列表
    #[test]
    fn list_filters_by_tag_and_distinct_tags() {
        // 同上：守卫必须绑定持有，防Linux下目录被提前删除
        let dir = tempfile::tempdir().unwrap();
        let repo = MonitorRepository::new(Arc::new(test_pool(dir.path())));
        let with_tag = |name: &str, tag: Option<&str>| MonitorConfigInsert {
            name: Some(name.to_string()),
            target: format!("https://{name}.com"),
            method: Some("GET".to_string()),
            monitor_type: "HTTP".to_string(),
            interval_ms: Some(5000),
            timeout_ms: 5000,
            config_json: Some("{}".to_string()),
            enabled: 1,
            tag: tag.map(str::to_string),
        };
        repo.create_monitor(&with_tag("a", Some("prod"))).unwrap();
        repo.create_monitor(&with_tag("b", Some("prod"))).unwrap();
        repo.create_monitor(&with_tag("c", Some("staging")))
            .unwrap();
        repo.create_monitor(&with_tag("d", None)).unwrap();

        // 按 tag 筛选
        let (list, total) = repo.list_monitors(None, Some("prod"), 1, 10).unwrap();
        assert_eq!(total, 2, "prod 应有两条");
        assert!(list.iter().all(|m| m.tag.as_deref() == Some("prod")));
        // 不带 tag：全部
        let (_, total_all) = repo.list_monitors(None, None, 1, 10).unwrap();
        assert_eq!(total_all, 4);
        // 去重标签：prod / staging（None 不计），升序
        let tags = repo.get_distinct_tags().unwrap();
        assert_eq!(tags, vec!["prod".to_string(), "staging".to_string()]);
    }

    #[test]
    fn delete_monitor_cascades_results_and_alert_state() {
        let dir = tempfile::tempdir().unwrap();
        let pool = Arc::new(test_pool(dir.path()));
        let repo = MonitorRepository::new(pool.clone());
        let created = repo.create_monitor(&insert("t-cascade")).unwrap();
        // 级联对象：检查结果 + 告警抑制状态
        {
            use crate::database::models::CheckResultModelInsert;
            use crate::database::repositories::alert_state_repo::AlertStateRepository;
            use crate::database::repositories::result_repo::CheckResultRepository;
            CheckResultRepository::new(pool.clone())
                .insert_check_result(&CheckResultModelInsert {
                    monitor_id: created.id,
                    monitor_type: "HTTP".to_string(),
                    status: 1,
                    response_time: 5,
                    metadata_json: None,
                })
                .unwrap();
            AlertStateRepository::new(pool.clone())
                .set_alerting(created.id, true)
                .unwrap();
        }
        // get_connection已开启foreign_keys：删监控应级联清理两张表
        assert_eq!(repo.delete_monitor(created.id).unwrap(), 1);
        {
            use crate::database::repositories::alert_state_repo::AlertStateRepository;
            use crate::database::repositories::result_repo::CheckResultRepository;
            let (_, total) = CheckResultRepository::new(pool.clone())
                .get_check_results(Some(created.id), 1, 10)
                .unwrap();
            assert_eq!(total, 0, "check_result应被级联删除");
            assert!(
                !AlertStateRepository::new(pool)
                    .get_alerting(created.id)
                    .unwrap(),
                "alert_state应被级联删除"
            );
        }
    }
}
