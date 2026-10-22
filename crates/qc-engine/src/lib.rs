//! QuickCopy 复制引擎。阶段 1 的粘贴接管和阶段 2 的暂存框共用它。
//!
//! 用法：`prepare()` 扫描出计划（总量、冲突），问完冲突后 `execute()` 多线程执行；
//! 做完的操作记进 `undo` 日志，可以撤销。

mod exec;
mod plan;
pub mod session;
pub mod ticket;
pub mod undo;
mod win;

pub use exec::{execute, Backup, Options, Progress, Report, Resolution};
pub use plan::{copy_name, expand, numbered_name, plan, prepare, rename_plan, same_path, top_item, DepDir, DepSkip, FileItem, Job, Mode, Plan, TopItem, DEP_ASK_MIN_FILES, DEP_DIRS};
pub use win::same_volume;

/// 达到这个大小的文件走无缓冲复制，并按块汇报进度。
pub const BIG_FILE: u64 = 64 * 1024 * 1024;
