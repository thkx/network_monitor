// 定义一个csv_logger.rs模块
use chrono::Local;
use prettytable::csv::Writer;
use std::error::Error;
use std::fs::OpenOptions; // 文件打开参数

// 定义记录结果的结构体
#[derive(Debug, Clone)]
pub struct UrlLogResult {
    pub check_id: String, // 本次检查的唯一ID（对应CheckResult.id，用于关联数据库与日志）
    pub url: String,
    pub status: bool,
    pub response_time: u128,
    pub status_code: Option<u16>,
}

// 日志核心struct 用于后续的日志操作
pub struct CsvLogger {
    file_name: String,
}

impl CsvLogger {
    pub fn new(file_name: &str) -> Self {
        //判断一下 当时的file_name是否存在，不存在就创建并写入表头
        if !std::path::Path::new(&file_name).exists() {
            let mut wtr = Writer::from_path(file_name).unwrap();
            wtr.write_record(["时间", "检查ID", "网址", "状态", "响应时间(ms)", "状态码"])
                .unwrap();
            wtr.flush().unwrap();
        }
        CsvLogger {
            file_name: file_name.to_string(),
        }
    }
    // 读写日志内容
    pub fn log(&self, result: UrlLogResult) -> Result<(), Box<dyn Error>> {
        let mut wtr = Writer::from_writer(
            OpenOptions::new() // 创建一个新的文件打开选项构建器
                .append(true) // 以追加的模式打开，不覆盖现有内容
                .create(true) // 如果文件不存在则创建
                .open(&self.file_name)?, // ？ 跟 unwrap  都用于处理Result 跟 Option，都是如果OK 或 Some 直接返回值，但是如果Err 或 None ？会返回错误 不会直接Panic，但是unwrap会直接Panic。
        );
        let code = result
            .status_code
            .map_or("None".to_string(), |x| x.to_string());
        wtr.write_record(&[
            Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            result.check_id,
            result.url,
            if result.status {
                "Success".to_string()
            } else {
                "Failed".to_string()
            },
            result.response_time.to_string(),
            code,
        ])?;
        wtr.flush()?;
        Ok(())
    }
}
