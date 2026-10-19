//! 命令行入口，测速和调试用：
//! `qc [--threads N] [--on-conflict skip|replace|keep] [--move] [--record] <源>... <目标文件夹>`
//! `qc --undo`：撤销日志里最近一条。`--record` 才记撤销日志，免得测速的复制混进资源管理器的 Ctrl+Z。

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use qc_engine::undo::{self, Op};
use qc_engine::{execute, prepare, Job, Mode, Options, Progress, Resolution};

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut opts = Options::default();
    let mut res = Resolution::Skip;
    let flag = |args: &mut Vec<String>, f: &str| args.iter().position(|a| a == f).map(|i| args.remove(i)).is_some();
    if flag(&mut args, "--undo") {
        let Some(op) = undo::last_undoable() else {
            eprintln!("没有可撤销的操作");
            return ExitCode::from(2);
        };
        let r = undo::undo(&op, &Progress::default());
        println!("撤销 {:?} 到 {}：{} 项完成 失败 {}", op.mode, op.dest.display(), r.copied_files, r.failures.len());
        for (p, e) in r.failures.iter().take(10) {
            eprintln!("  失败 {}：{e}", p.display());
        }
        return if r.failures.is_empty() { ExitCode::SUCCESS } else { ExitCode::from(1) };
    }
    let mode = if flag(&mut args, "--move") { Mode::Move } else { Mode::Copy };
    let record = flag(&mut args, "--record");
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
        eprintln!("用法：qc [--threads N] [--on-conflict skip|replace|keep] [--move] [--record] <源>... <目标文件夹> | qc --undo");
        return ExitCode::from(2);
    }
    let dest = PathBuf::from(args.pop().unwrap());
    let job = Job { sources: args.into_iter().map(PathBuf::from).collect(), dest, mode };

    let t0 = Instant::now();
    let p = match prepare(&job) {
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
    if record && r.changed_anything() {
        undo::record(Op::from_report(&job, &r)).expect("记撤销日志失败");
    }
    println!(
        "{}扫描 {:.2}s  执行 {:.2}s  文件 {}/{}  {:.1} MB  冲突 {}  跳过 {}  失败 {}  备份 {}",
        if p.rename { "[改名] " } else { "" },
        t_plan.as_secs_f64(),
        r.elapsed.as_secs_f64(),
        r.copied_files,
        p.files.len(),
        r.copied_bytes as f64 / 1048576.0,
        p.conflict_count(),
        r.skipped.len(),
        r.failures.len(),
        r.backups.len()
    );
    println!("内存：扫描后 {mem_plan}；结束 {}", qc_engine::ticket::peak_memory());
    for (path, e) in r.failures.iter().chain(&p.errors).take(10) {
        eprintln!("  失败 {}：{e}", path.display());
    }
    if r.failures.is_empty() && p.errors.is_empty() { ExitCode::SUCCESS } else { ExitCode::from(1) }
}
