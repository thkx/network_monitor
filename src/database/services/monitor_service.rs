use crate::database::models::{MonitorConfigInsert, MonitorConfigModel, MonitorConfigUpdate};
use crate::database::repositories::monitor_repo::MonitorRepository;
use diesel::result::Error;
use std::sync::Arc;

#[derive(Clone)]
pub struct MonitorService {
    // 存储对Repo层对象的引用
    repo: Arc<MonitorRepository>,
}
impl MonitorService {
    pub fn new(repo: MonitorRepository) -> Self {
        MonitorService {
            repo: Arc::new(repo),
        }
    }
    // 这里的话 我们没有特殊的逻辑 所以比较简单
    // 如果后续 我们把告警规则、告警配置单独拎出来的话 对应的业务逻辑就需要在这里进行处理
    pub fn get_monitors_by_enabled(
        &self,
        enabled_flag: bool,
        page: i64,
        page_size: i64,
    ) -> Result<(Vec<MonitorConfigModel>, i64), Error> {
        self.repo
            .get_monitors_by_enabled(enabled_flag, page, page_size)
    }

    // 分页查询全部监控配置（不筛选启停状态，供列表页展示禁用项）
    pub fn get_monitors_paged(
        &self,
        page: i64,
        page_size: i64,
    ) -> Result<(Vec<MonitorConfigModel>, i64), Error> {
        self.repo.get_monitors_paged(page, page_size)
    }

    // 启用/禁用监控配置（其余字段保持不变）
    pub fn set_enabled(&self, id: i32, enabled: bool) -> Result<usize, Error> {
        self.repo.update_monitor(
            id,
            &MonitorConfigUpdate {
                enabled: Some(if enabled { 1 } else { 0 }),
                name: None,
                target: None,
                method: None,
                monitor_type: None,
                interval_ms: None,
                timeout_ms: None,
                config_json: None,
                tag: None,
            },
        )
    }

    // 查询所有启用的监控配置（不分页，供Server模式调度器加载）
    pub fn get_all_enabled(&self) -> Result<Vec<MonitorConfigModel>, Error> {
        self.repo.get_all_enabled()
    }

    // 全量查询所有监控配置（/api/status 控制台聚合视图用，不受分页上限影响）
    pub fn get_all_monitors(&self) -> Result<Vec<MonitorConfigModel>, Error> {
        self.repo.get_all()
    }

    // 按ID查询监控配置
    pub fn get_monitor_by_id(&self, id: i32) -> Result<Option<MonitorConfigModel>, Error> {
        self.repo.get_monitor_by_id(id)
    }

    // 按名称查询监控配置（导入去重用）
    pub fn find_by_name(&self, name: &str) -> Result<Option<MonitorConfigModel>, Error> {
        self.repo.find_by_name(name)
    }

    // 新增监控配置
    pub fn create_monitor(
        &self,
        insert: &MonitorConfigInsert,
    ) -> Result<MonitorConfigModel, Error> {
        self.repo.create_monitor(insert)
    }

    // 更新监控配置
    pub fn update_monitor(&self, id: i32, update: &MonitorConfigUpdate) -> Result<usize, Error> {
        self.repo.update_monitor(id, update)
    }

    // 删除监控配置
    pub fn delete_monitor(&self, id: i32) -> Result<usize, Error> {
        self.repo.delete_monitor(id)
    }
}
