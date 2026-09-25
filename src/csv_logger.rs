// 定义一个csv_logger.rs模块
use chrono::Local;
use prettytable::csv::Writer;
use std::error::Error;
use std::fs::{File, OpenOptions};
use std::sync::Mutex;

// 定义记录结果的结构体
#[derive(Debug, Clone)]
pub struct UrlLogResult {
    pub check_id: String, // 本次检查的唯一ID（对应CheckResult.id，用于关联数据库与日志）
    pub monitor: String,  // 监控项展示名（target 或类型名），此前误名为 url——非HTTP监控无URL
    pub status: bool,
    pub response_time: u128,
    pub status_code: Option<u16>,
}

// 常驻写句柄 + 当前所在日期：跨天时整体替换
struct Inner {
    date: String,      // 当前 writer 对应的日期 YYYY-MM-DD
    writer: Writer<File>, // 常驻写句柄（此前每条日志都 open+flush+close，改为句柄常驻）
}

// 日志核心struct 用于后续的日志操作
// 按日滚动：实际写入的是 <base>-YYYY-MM-DD.<ext> 派生文件；句柄常驻，仅跨天时重开。
pub struct CsvLogger {
    base_path: String,
    inner: Mutex<Inner>,
}

// 表头：新建（空）文件时写入
const HEADER: [&str; 6] = ["时间", "检查ID", "网址", "状态", "响应时间(ms)", "状态码"];

// 由基础路径与日期派生当日文件名：把日期插到扩展名之前（无扩展名则直接追加）。
// monitor_log.csv + 2025-01-01 -> monitor_log-2025-01-01.csv
fn dated_path(base: &str, date: &str) -> String {
    let last_sep = base.rfind(['/', '\\']);
    match base.rfind('.') {
        // 仅当 '.' 在最后一个路径分隔符之后（即属于文件名而非目录）时才视为扩展名
        Some(dot) if last_sep.is_none_or(|s| dot > s) => {
            format!("{}-{}{}", &base[..dot], date, &base[dot..])
        }
        _ => format!("{}-{}", base, date),
    }
}

// 打开（或创建）当日文件；新建或空文件时写入表头
fn open_dated(base: &str, date: &str) -> Result<Writer<File>, Box<dyn Error>> {
    let path = dated_path(base, date);
    // 文件不存在或长度为0都需要补表头（防止空文件缺表头）
    let need_header = std::fs::metadata(&path).map(|m| m.len() == 0).unwrap_or(true);
    let file = OpenOptions::new()
        .append(true) // 追加，不覆盖已有内容
        .create(true) // 不存在则创建
        .open(&path)?;
    let mut wtr = Writer::from_writer(file);
    if need_header {
        wtr.write_record(HEADER)?;
        wtr.flush()?;
    }
    Ok(wtr)
}

impl CsvLogger {
    pub fn new(base_path: &str) -> Self {
        let date = Local::now().format("%Y-%m-%d").to_string();
        // 启动即建立常驻句柄；打开失败与此前 unwrap 语义一致地视为启动错误
        let writer = open_dated(base_path, &date).expect("CSV日志初始化失败");
        CsvLogger {
            base_path: base_path.to_string(),
            inner: Mutex::new(Inner { date, writer }),
        }
    }

    // 读写日志内容：使用常驻句柄追加一条记录，跨天时先滚动到新文件
    pub fn log(&self, result: UrlLogResult) -> Result<(), Box<dyn Error>> {
        let date = Local::now().format("%Y-%m-%d").to_string();
        self.log_at(result, &date)
    }

    // 以显式日期写入（便于单测跨天滚动，无需等待自然跨天）
    fn log_at(&self, result: UrlLogResult, date: &str) -> Result<(), Box<dyn Error>> {
        let mut inner = self.inner.lock().expect("CSV日志锁中毒");
        if inner.date != date {
            // 跨天滚动：切换到新日期文件；旧 writer 被替换后其文件句柄随之关闭
            inner.writer = open_dated(&self.base_path, date)?;
            inner.date = date.to_string();
        }
        let code = result
            .status_code
            .map_or("None".to_string(), |x| x.to_string());
        inner.writer.write_record(&[
            Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            result.check_id,
            result.monitor,
            if result.status {
                "Success".to_string()
            } else {
                "Failed".to_string()
            },
            result.response_time.to_string(),
            code,
        ])?;
        // 常驻句柄仍逐条 flush 以保证监控日志的持久性（省去的是每条的 open/close）
        inner.writer.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(id: &str) -> UrlLogResult {
        UrlLogResult {
            check_id: id.to_string(),
            monitor: "t".to_string(),
            status: true,
            response_time: 1,
            status_code: Some(200),
        }
    }

    #[test]
    fn dated_path_inserts_date_before_extension() {
        assert_eq!(dated_path("monitor_log.csv", "2025-01-01"), "monitor_log-2025-01-01.csv");
        assert_eq!(dated_path("logs/m.csv", "2025-01-01"), "logs/m-2025-01-01.csv");
        assert_eq!(dated_path("noext", "2025-01-01"), "noext-2025-01-01");
        // 目录含点、文件名无扩展名：不应把目录的点当扩展名
        assert_eq!(dated_path("a.b/log", "2025-01-01"), "a.b/log-2025-01-01");
    }

    #[test]
    fn rolls_over_to_new_file_across_days() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("monitor_log.csv");
        let logger = CsvLogger::new(base.to_str().unwrap());

        logger.log_at(rec("a"), "2025-01-01").unwrap();
        logger.log_at(rec("b"), "2025-01-02").unwrap(); // 跨天

        let f1 = dir.path().join("monitor_log-2025-01-01.csv");
        let f2 = dir.path().join("monitor_log-2025-01-02.csv");
        assert!(f1.exists(), "第一天文件应存在");
        assert!(f2.exists(), "跨天应产生新文件");

        let c1 = std::fs::read_to_string(&f1).unwrap();
        let c2 = std::fs::read_to_string(&f2).unwrap();
        assert!(c1.contains("检查ID"), "首日文件应含表头");
        assert!(c1.contains(",a,"), "首日文件应含首日记录");
        assert!(!c1.contains(",b,"), "首日文件不应含次日记录");
        assert!(c2.contains(",b,"), "次日文件应含次日记录");
    }
}
