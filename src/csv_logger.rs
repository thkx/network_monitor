// 定义一个csv_logger.rs模块
use chrono::{Local, NaiveDate};
use prettytable::csv::Writer;
use std::error::Error;
use std::fs::{File, OpenOptions};
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

// 批量/定时刷盘阈值（ADR-0004 异步线程模型配套）：
// 写记录仅进 csv::Writer 的内存缓冲（append，近似非阻塞），
// 攒满 FLUSH_EVERY 条、或距上次刷盘超过 FLUSH_INTERVAL 才做一次 flush（写系统调用）。
// 这样把原先“每条一次 flush 系统调用”从结果消费热路径上移除；
// 崩溃时最多丢最后不足 1 秒的 CSV 记录（数据库为权威存储，CSV 为辅助）。
const FLUSH_EVERY: usize = 64;
const FLUSH_INTERVAL: Duration = Duration::from_millis(1000);

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
    date: String,         // 当前 writer 对应的日期 YYYY-MM-DD
    writer: Writer<File>, // 常驻写句柄（此前每条日志都 open+flush+close，改为句柄常驻）
    pending: usize,       // 自上次刷盘以来累积未 flush 的记录数
    last_flush: Instant,  // 上次刷盘时刻（用于定时刷盘）
}

// 日志核心struct 用于后续的日志操作
// 按日滚动：实际写入的是 <base>-YYYY-MM-DD.<ext> 派生文件；句柄常驻，仅跨天时重开。
pub struct CsvLogger {
    base_path: String,
    // CSV 保留天数（ADR-0007 D6）：与数据库保留策略对称，防止按日滚动的 CSV 文件
    // 无限堆积撑满磁盘、连带打死权威数据库。None=不清理（保留全部）。
    // 由环境变量 CSV_RETENTION_DAYS 配置：缺省复用 RESULT_RETENTION_DAYS（默认30），
    // 显式设为 0 或负数表示永久保留（关闭清理）。
    retention_days: Option<i64>,
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
    let need_header = std::fs::metadata(&path)
        .map(|m| m.len() == 0)
        .unwrap_or(true);
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
        Self::with_retention(base_path, retention_days_from_env())
    }

    // 带显式保留天数的构造（供测试直接注入；生产走 new 读环境变量）
    pub fn with_retention(base_path: &str, retention_days: Option<i64>) -> Self {
        let date = Local::now().format("%Y-%m-%d").to_string();
        // 启动即建立常驻句柄；打开失败与此前 unwrap 语义一致地视为启动错误
        let writer = open_dated(base_path, &date).expect("CSV日志初始化失败");
        let logger = CsvLogger {
            base_path: base_path.to_string(),
            retention_days,
            inner: Mutex::new(Inner {
                date: date.clone(),
                writer,
                pending: 0,
                last_flush: Instant::now(),
            }),
        };
        // 启动时清理一次过期的按日 CSV 文件（当天文件不受影响）
        logger.cleanup_old_files(&date);
        logger
    }

    // 读写日志内容：使用常驻句柄追加一条记录，跨天时先滚动到新文件
    pub fn log(&self, result: UrlLogResult) -> Result<(), Box<dyn Error>> {
        let date = Local::now().format("%Y-%m-%d").to_string();
        self.log_at(result, &date)
    }

    // 以显式日期写入（便于单测跨天滚动，无需等待自然跨天）
    fn log_at(&self, result: UrlLogResult, date: &str) -> Result<(), Box<dyn Error>> {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut rolled_over = false;
        if inner.date != date {
            // 跨天滚动：切到新日期文件前，先刷掉旧句柄缓冲中残留的记录，避免丢数据；
            // 旧 writer 被替换后其文件句柄随之关闭
            let _ = inner.writer.flush();
            inner.writer = open_dated(&self.base_path, date)?;
            inner.date = date.to_string();
            inner.pending = 0;
            inner.last_flush = Instant::now();
            rolled_over = true;
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
        // 批量/定时刷盘：写记录只进内存缓冲，攒满或超时才 flush（见文件头常量说明），
        // 把每条一次的 flush 系统调用从消费热路径上移除。
        inner.pending += 1;
        if inner.pending >= FLUSH_EVERY || inner.last_flush.elapsed() >= FLUSH_INTERVAL {
            inner.writer.flush()?;
            inner.pending = 0;
            inner.last_flush = Instant::now();
        }
        drop(inner);
        // 跨天滚动后顺带清理过期的旧日 CSV 文件（ADR-0007 D6），避免无限堆积；
        // 放在释放写锁之后做，不阻塞写路径
        if rolled_over {
            self.cleanup_old_files(date);
        }
        Ok(())
    }

    // 显式刷盘：把内存缓冲中的记录写入磁盘。
    // 由消费循环按秒级节奏调用、以及优雅关停前调用，保证 CSV 的及时持久化。
    pub fn flush(&self) -> Result<(), Box<dyn Error>> {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        inner.writer.flush()?;
        inner.pending = 0;
        inner.last_flush = Instant::now();
        Ok(())
    }

    // 清理过期的按日 CSV 文件（ADR-0007 D6）：扫描基础路径所在目录，删除文件名形如
    // <stem>-YYYY-MM-DD<ext> 且日期早于 (today - retention_days) 的文件。
    // retention_days 为 None 或 <=0 时不清理；当天文件因日期不早于截止永不被删。
    // 删除失败仅记录 warn，不影响日志写入主流程。
    fn cleanup_old_files(&self, today: &str) {
        let Some(days) = self.retention_days.filter(|d| *d > 0) else {
            return;
        };
        let Ok(today_date) = NaiveDate::parse_from_str(today, "%Y-%m-%d") else {
            return;
        };
        let cutoff = today_date - chrono::Duration::days(days);
        // 目录与"文件名前后缀"：用 dated_path 的插入规则推导，避免误删同目录其它文件
        let base = Path::new(&self.base_path);
        let dir = base.parent().filter(|p| !p.as_os_str().is_empty());
        let dir = dir.unwrap_or_else(|| Path::new("."));
        let Some(file_base) = base.file_name().and_then(|s| s.to_str()) else {
            return;
        };
        // 以标记位切出日期前后缀，如 "monitor_log-" / ".csv"
        const MARKER: &str = "\u{0}DATE\u{0}";
        let marked = dated_path(file_base, MARKER);
        let Some((name_prefix, name_suffix)) = marked.split_once(MARKER) else {
            return;
        };
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("CSV 清理读取目录失败: {}", e);
                return;
            }
        };
        for entry in entries.flatten() {
            let fname = entry.file_name();
            let Some(fname) = fname.to_str() else {
                continue;
            };
            // 必须精确匹配 "<prefix>DATE<suffix>" 结构，中间段恰为合法日期
            let Some(mid) = fname
                .strip_prefix(name_prefix)
                .and_then(|s| s.strip_suffix(name_suffix))
            else {
                continue;
            };
            let Ok(file_date) = NaiveDate::parse_from_str(mid, "%Y-%m-%d") else {
                continue;
            };
            if file_date < cutoff {
                match std::fs::remove_file(entry.path()) {
                    Ok(()) => tracing::info!("已清理过期 CSV 日志文件: {}", fname),
                    Err(e) => tracing::warn!("清理过期 CSV 文件 {} 失败: {}", fname, e),
                }
            }
        }
    }
}

// 从环境变量解析 CSV 保留天数（ADR-0007 D6）：
// CSV_RETENTION_DAYS 优先；缺省复用 RESULT_RETENTION_DAYS（与数据库保留对称），
// 两者都缺省则默认 30 天；显式 <=0 表示永久保留（关闭清理，返回 None）
fn retention_days_from_env() -> Option<i64> {
    let raw = std::env::var("CSV_RETENTION_DAYS")
        .or_else(|_| std::env::var("RESULT_RETENTION_DAYS"))
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .unwrap_or(30);
    if raw > 0 { Some(raw) } else { None }
}

impl Drop for CsvLogger {
    // 兜底刷盘：进程结束/日志器析构时尽力把缓冲写盘（Once/Monitor 模式无显式 flush 节奏时的保障）
    fn drop(&mut self) {
        if let Ok(mut inner) = self.inner.lock() {
            let _ = inner.writer.flush();
        }
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
        assert_eq!(
            dated_path("monitor_log.csv", "2025-01-01"),
            "monitor_log-2025-01-01.csv"
        );
        assert_eq!(
            dated_path("logs/m.csv", "2025-01-01"),
            "logs/m-2025-01-01.csv"
        );
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
        // 批量刷盘后记录可能仍在内存缓冲：显式 flush 再读取校验
        logger.flush().unwrap();

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

    // D6：启动清理删除早于保留窗口的按日 CSV，保留窗口内文件与非匹配文件不受影响
    #[test]
    fn cleanup_removes_files_older_than_retention() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("monitor_log.csv");
        let base_str = base.to_str().unwrap();
        let today = chrono::Local::now().date_naive();
        let old = today - chrono::Duration::days(40);
        let recent = today - chrono::Duration::days(3);
        let old_f = dir
            .path()
            .join(format!("monitor_log-{}.csv", old.format("%Y-%m-%d")));
        let recent_f = dir
            .path()
            .join(format!("monitor_log-{}.csv", recent.format("%Y-%m-%d")));
        let unrelated = dir.path().join("other.csv");
        std::fs::write(&old_f, "x").unwrap();
        std::fs::write(&recent_f, "x").unwrap();
        std::fs::write(&unrelated, "x").unwrap();

        // 保留30天：构造时启动清理
        let _logger = CsvLogger::with_retention(base_str, Some(30));
        assert!(!old_f.exists(), "40天前文件应被清理");
        assert!(recent_f.exists(), "3天内文件应保留");
        assert!(unrelated.exists(), "不匹配命名的文件不应被动");
    }

    // D6：retention 为 None（关闭清理）时，再老的文件也保留
    #[test]
    fn cleanup_disabled_keeps_all_files() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("monitor_log.csv");
        let base_str = base.to_str().unwrap();
        let today = chrono::Local::now().date_naive();
        let old = today - chrono::Duration::days(400);
        let old_f = dir
            .path()
            .join(format!("monitor_log-{}.csv", old.format("%Y-%m-%d")));
        std::fs::write(&old_f, "x").unwrap();
        let _logger = CsvLogger::with_retention(base_str, None);
        assert!(old_f.exists(), "关闭清理时历史文件应全部保留");
    }
}
