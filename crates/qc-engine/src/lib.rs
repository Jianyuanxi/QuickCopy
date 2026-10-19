//! QuickCopy 复制引擎。阶段 1 的粘贴接管和阶段 2 的暂存框共用它。
//!
//! 用法：`plan()` 扫描出计划（总量、冲突），问完冲突后 `execute()` 多线程执行。

mod exec;
mod plan;
mod win;

pub use exec::{execute, Options, Progress, Report, Resolution};
pub use plan::{copy_name, numbered_name, plan, same_path, FileItem, Job, Mode, Plan, TopItem};

/// 达到这个大小的文件走无缓冲复制，并按块汇报进度。
pub const BIG_FILE: u64 = 64 * 1024 * 1024;
