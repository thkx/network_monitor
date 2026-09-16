//
use clap::{ArgAction, Parser, ValueEnum, ValueHint};
use std::path::PathBuf;

/// A simple file manager written in Rust
#[derive(Parser, Debug, Clone)] // <- 增加 Clone
#[command(name="fm", author, version, about, long_about = None)]
pub struct Args {
    /// 位置参数：按名字包含匹配（可选）
    /// 示例： fm main.rs
    #[arg(value_name = "NAME")]
    pub name: Option<String>,

    /// 正则匹配 （可选）
    /// 示例： fm -r ".rs" 或者 fm -r "^main.*\.rs$"
    #[arg(short = 'r', long = "regex", value_name = "REGEX")]
    pub regex: Option<String>,

    /// 支持展示隐藏的文件 （可选）
    /// 示例： fm -a
    #[arg(short='a', long="all", action=ArgAction::SetTrue)]
    pub all: bool,

    /// 是否递归查询（可选）
    // ArgAction::SetTrue 表示：当该选项出现在命令行里时，把对应的布尔字段设置为 true（不出现则保持默认，通常为 false）。适用于“开关型”无值参数。
    /// 示例： fm -r ".rs"
    #[arg(short='R', long="recursive", action=ArgAction::SetTrue)]
    pub recursive: bool,

    /// 递归层级 （可选）
    /// 示例： fm -r ".rs" -d 2
    #[arg(short='d', long="depth", num_args= 0.. , default_missing_value = "10", value_name = "DEPTH")]
    pub depth: Option<u8>,

    /// 排序字段 （可选, 支持多值： name,size,time,type）
    /// 示例：
    ///      fm -r ".rs" -s        ==> 默认值为 name 等同于 fm -r ".rs" -s name
    ///    fm -r ".rs" -s       ===> 按 size 进行排序
    ///    fm -r ".rs" -s time size name  ==> 依次按 time,size,name 进行排序
    #[arg(short='s', long="sort", value_enum, num_args = 0.. , default_missing_value = "name", value_name = "SORT_FIELD")]
    pub sort: Vec<SortField>,

    /// 显示统计信息（可选,支持多值，仅出现 -t 不写值时，默认为all）
    /// 示例：
    ///    fm -r ".rs" -t        ==> 等同于 fm -r ".rs" -t all
    ///     fm -r ".rs" -t all    ==> 显示所有统计
    ///    fm -r ".rs" -t size time type ==> 显示大小，时间，类型统计
    ///    fm -r ".rs" -t size   ==> 仅显示总大小统计
    ///    fm -r ".rs" -t type  ==> 仅显示类型统计
    #[arg(short='t', long="stats", value_enum, num_args = 0.. , default_missing_value = "all", default_value = "name", value_name = "STATS_FIELD")]
    pub stats: Vec<StatsField>,

    /// 输出格式配置 （可选）： csv,   默认为 csv
    /// 示例： fm -r ".rs" -o json
    ///    fm -r ".rs" -o csv
    #[arg(short='o', long="output", value_enum, num_args = 0.., default_missing_value = "csv", value_name = "FORMAT")]
    pub output: Option<OutputFormat>,

    /// 工作目录 （可选）
    // 示例： fm -r ".rs" -w /home/user/work
    #[arg(short='w', long="workdir", value_hint=ValueHint::DirPath, value_name = "WORKDIR")]
    pub workdir: Option<PathBuf>,
}

#[derive(Clone, Debug, Copy, ValueEnum)]
pub enum SortField {
    Name,
    Size,
    Time,
    Type,
}

#[derive(Clone, Debug, Copy, ValueEnum)]
pub enum OutputFormat {
    Csv,
    Line,
}

#[derive(Clone, Debug, Copy, ValueEnum)]
pub enum StatsField {
    All,
    Name,
    Time,
    Size,
    Type,
}
