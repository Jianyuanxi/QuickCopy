//! 命令行入口，测速和调试用：`qc [--threads N] [--on-conflict skip|replace|keep] <源>... <目标文件夹>`

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use qc_engine::{execute, plan, Job, Mode, Options, Progress, Resolution};

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut opts = Options::default();
    let mut res = Resolution::Skip;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--threads" => {
                opts.threads = args[i + 1].parse().expect("--threads 要跟数字");
                args.drain(i..i + 2);
            }
            "--on-conflict" => {
                res = match args[i + 1].as_str() {
                    "replace" => Resolution::Replace,
                    "keep" => Resolution::KeepBoth,
                    _ => Resolution::Skip,
                };
                args.drain(i..i + 2);
            }
            _ => i += 1,
        }
    }
    if args.len() < 2 {
        eprintln!("用法：qc [--threads N] [--on-conflict skip|replace|keep] <源>... <目标文件夹>");
        return ExitCode::from(2);
    }
    let dest = PathBuf::from(args.pop().unwrap());
    let job = Job { sources: args.into_iter().map(PathBuf::from).collect(), dest, mode: Mode::Copy };

    let t0 = Instant::now();
    let p = match plan(&job) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };
    let t_plan = t0.elapsed();
    let mem_plan = qc_engine::ticket::peak_memory();
    let prog = Progress::default();
    let r = execute(&p, &opts, &prog, &|_| res);
    println!(
        "扫描 {:.2}s  复制 {:.2}s  文件 {}/{}  {:.1} MB  冲突 {}  跳过 {}  失败 {}",
        t_plan.as_secs_f64(),
        r.elapsed.as_secs_f64(),
        r.copied_files,
        p.files.len(),
        r.copied_bytes as f64 / 1048576.0,
        p.conflict_count(),
        r.skipped.len(),
        r.failures.len()
    );
    println!("内存：扫描后 {mem_plan}；结束 {}", qc_engine::ticket::peak_memory());
    for (path, e) in r.failures.iter().chain(&p.errors).take(10) {
        eprintln!("  失败 {}：{e}", path.display());
    }
    if r.failures.is_empty() && p.errors.is_empty() { ExitCode::SUCCESS } else { ExitCode::from(1) }
}
