// 定义相关的工具函数
use crate::domain::MonitorDefinition;
use std::fs;
use std::path::PathBuf;

/// 根据文件名获取当前工作目录下的完整路径
pub fn get_file_path(file_name: &str) -> PathBuf {
    let current_dir = std::env::current_dir().unwrap();
    current_dir.join(file_name)
}

/// 读取JSON配置文件，解析为自定义监控配置列表
pub fn read_json_file(
    file_path: &str,
) -> Result<Vec<MonitorDefinition>, Box<dyn std::error::Error>> {
    let file_path = get_file_path(file_path);
    let content = fs::read_to_string(file_path)?;
    let json_data: Vec<MonitorDefinition> = serde_json::from_str(&content)?;
    Ok(json_data)
}
