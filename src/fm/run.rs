// fm - 文件管理器
// A simple file manager written in Rust

use clap::Parser;

use crate::fm::utils::{get_file_list, print_file_list_row, print_file_list_table};
use crate::fm::{Args, OutputFormat};

pub fn run() {
    let args_opts = Args::parse();
    let file_list = get_file_list(&args_opts, None).unwrap();

    println!("{:?}\n当前目录为{:#?}", args_opts, file_list);

    // 打印参数 和格式化输出结果
    let stats_fields = &args_opts.stats;

    // 设置输出的格式
    let output_format = args_opts.output.unwrap();
    if let OutputFormat::Csv = output_format {
        print_file_list_table(&file_list, stats_fields);
    } else {
        // 拼接字符串 按行输出
        print_file_list_row(&file_list, stats_fields);
    }
}
