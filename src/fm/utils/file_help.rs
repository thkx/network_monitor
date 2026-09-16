use crate::fm::args::Args;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Local};
use std::time::SystemTime;

use regex::Regex;
/// 获取当前的工作目录
pub fn get_current_dir() -> Result<PathBuf, std::io::Error> {
    std::env::current_dir()
}

fn fmt_system_time(system_time: SystemTime) -> String {
    let datetime: DateTime<Local> = system_time.into();
    datetime.format("%Y-%m-%d %H:%M:%S").to_string()
}

// 定义文件的元数据 struct
#[derive(Debug, Clone)]
pub struct FileMetaData {
    name: String,
    path: PathBuf,
    size: u64,
    time: String,
    is_dir: bool,
    file_type: String,
}

// 查询当前目录下的文件和文件夹列表
pub fn get_file_list(
    args: &Args,
    p_name: Option<String>,
) -> Result<Vec<FileMetaData>, std::io::Error> {
    let mut file_list = Vec::new();

    // 判断当前的args中 是否有工作目录，如果没有则使用当前工作目录
    // as_ref() 是因为 workdir 是 Option<PathBuf>，需要转换为 Option<&PathBuf> 以便后续使用
    let cwd = get_current_dir()?;
    let workdir = args.workdir.as_ref().unwrap_or(&cwd);
    for entry in std::fs::read_dir(workdir)? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        let name = entry.file_name().to_string_lossy().to_string();
        let is_dir = metadata.is_dir();
        // 判断是否隐藏.开头文件
        if name.starts_with(".") && !args.all {
            continue;
        }
        // 递归查询x层级
        if args.recursive && is_dir {
            if args.depth.is_some() && args.depth.unwrap() == 0 {
                continue;
            }
            // 这里可以添加递归查询的逻辑，目前仅支持一层
            let sub_args = Args {
                depth: args.depth.map(|d| if d > 0 { d - 1 } else { 0 }), // 递减层级
                workdir: Some(entry.path()),
                ..args.clone()
            };

            let p_name = if let Some(ref p) = p_name {
                Some(p.clone() + "/" + &name)
            } else {
                Some(name.clone())
            };
            let sub_file_list = get_file_list(&sub_args, p_name)?;
            file_list.extend(sub_file_list);
        }

        // 判断一下 如果不包含 name 则跳过
        if let Some(ref needle) = args.name
            && !name.contains(needle) {
                continue;
            }
        // 正则表达式匹配
        if let Some(ref regex_str) = args.regex {
            let re = Regex::new(regex_str).unwrap();
            if !re.is_match(&name) {
                continue;
            }
        }
        let show_name = if let Some(ref p_name) = p_name {
            p_name.to_string() + "/" + name.as_str()
        } else {
            name.to_string()
        };

        file_list.push(FileMetaData {
            name: show_name,
            path: entry.path(),
            size: metadata.len(),
            time: metadata
                .modified()
                .map(fmt_system_time)
                .unwrap_or_else(|_| "-".to_string()),
            is_dir,
            file_type: get_file_extension(&entry.path(), is_dir).unwrap_or_else(|| "-".to_string()),
        });
    }

    // 排序字段
    if !args.sort.is_empty() {
        for sort_field in args.sort.iter().rev() {
            match sort_field {
                crate::fm::args::SortField::Name => {
                    file_list.sort_by_key(|a| a.name.to_lowercase());
                }
                crate::fm::args::SortField::Size => {
                    file_list.sort_by_key(|a| a.size);
                }
                crate::fm::args::SortField::Time => {
                    file_list.sort_by(|a, b| a.time.cmp(&b.time));
                }
                crate::fm::args::SortField::Type => {
                    file_list.sort_by(|a, b| {
                        a.file_type.to_lowercase().cmp(&b.file_type.to_lowercase())
                    });
                }
            }
        }
    }

    Ok(file_list)
}

// 获取当前文件的后缀名
pub fn get_file_extension(path: &Path, is_dir: bool) -> Option<String> {
    if is_dir {
        return None;
    }
    path.extension()
        .and_then(|ext| ext.to_str().map(|s| s.to_string()))
}

// 使用表格形式打印信息
pub fn print_file_list_table(
    file_list: &[FileMetaData],
    stats_fields: &[crate::fm::args::StatsField],
) {
    use prettytable::{Cell, Row, Table};
    let mut columns = vec![Cell::new("Name"), Cell::new("Is Directory")];
    stats_fields.iter().for_each(|field| match field {
        crate::fm::args::StatsField::Size => columns.push(Cell::new("Size")),
        crate::fm::args::StatsField::Time => columns.push(Cell::new("Modified Time")),
        crate::fm::args::StatsField::Type => columns.push(Cell::new("Type")),
        crate::fm::args::StatsField::All => {
            columns.push(Cell::new("Path"));
            columns.push(Cell::new("Size"));
            columns.push(Cell::new("Modified Time"));
            columns.push(Cell::new("Type"));
        }
        _ => {}
    });
    let mut table = Table::new();
    table.add_row(Row::new(columns));

    for file in file_list {
        let mut columns = vec![Cell::new(&file.name), Cell::new(&file.is_dir.to_string())];
        stats_fields.iter().for_each(|field| match field {
            crate::fm::args::StatsField::Size => columns.push(Cell::new(&file.size.to_string())),
            crate::fm::args::StatsField::Time => columns.push(Cell::new(&file.time)),
            crate::fm::args::StatsField::Type => columns.push(Cell::new(&file.file_type)),
            crate::fm::args::StatsField::All => {
                columns.push(Cell::new(&file.path.to_string_lossy()));
                columns.push(Cell::new(&file.size.to_string()));
                columns.push(Cell::new(&file.time));
                columns.push(Cell::new(&file.file_type));
            }
            _ => {}
        });
        table.add_row(Row::new(columns));
    }

    table.printstd();
}

// 使用行形式 打印信息
pub fn print_file_list_row(
    file_list: &[FileMetaData],
    stats_fields: &[crate::fm::args::StatsField],
) {
    stats_fields.iter().for_each(|field| match field {
        crate::fm::args::StatsField::Name => println!("name"),
        crate::fm::args::StatsField::Size => println!("Name\\Size\n "),
        crate::fm::args::StatsField::Time => println!("Name\\Time\n "),
        crate::fm::args::StatsField::Type => println!("Name\\Type\n "),
        crate::fm::args::StatsField::All => {
            println!("Name\\Size\\Modified Time\\Type\n ");
        } // _ => {}
    });
    for file in file_list {
        stats_fields.iter().for_each(|field| match field {
            crate::fm::args::StatsField::Name => println!("{}", file.name),
            crate::fm::args::StatsField::Size => println!("{}\\{}\n ", file.name, file.size),
            crate::fm::args::StatsField::Time => println!("{}\\{}\n ", file.name, file.time),
            crate::fm::args::StatsField::Type => println!("{}\\{}\n ", file.name, file.file_type),
            crate::fm::args::StatsField::All => {
                println!(
                    "{}\\{}\\{}\\{}\n ",
                    file.name, file.size, file.time, file.file_type
                );
            } // _ => {
              //     println!("\n ");
              // }
        });
    }
}
