//! 扫描阶段：把要复制/移动的东西展开成目录清单和文件清单，顺带算总量、找冲突。

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mode {
    Copy,
    Move,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Job {
    pub sources: Vec<PathBuf>,
    pub dest: PathBuf,
    pub mode: Mode,
}

/// 用户选中的一个顶层条目（文件或文件夹）。
#[derive(Clone, Debug)]
pub struct TopItem {
    pub src: PathBuf,
    pub dst: PathBuf,
    pub is_dir: bool,
    /// 目标位置已经有同名文件夹：要合并，里面的文件逐个查冲突。
    pub merge: bool,
    /// 目标位置已经有同名文件：整个条目是一个冲突。
    pub conflict: bool,
}

#[derive(Clone, Debug)]
pub struct FileItem {
    pub src: PathBuf,
    pub dst: PathBuf,
    pub size: u64,
    pub conflict: bool,
    pub top: u32,
}

#[derive(Debug, Default)]
pub struct Plan {
    pub tops: Vec<TopItem>,
    /// 需要新建的目标目录。
    pub dirs: Vec<PathBuf>,
    pub files: Vec<FileItem>,
    pub total_bytes: u64,
    /// 没有展开的目录符号链接 / 联接点。
    pub skipped_links: Vec<PathBuf>,
    pub errors: Vec<(PathBuf, String)>,
}

impl Plan {
    pub fn conflict_count(&self) -> usize {
        self.files.iter().filter(|f| f.conflict).count()
    }
}

pub fn same_path(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| {
        p.to_string_lossy()
            .trim_end_matches(['\\', '/'])
            .replace('/', "\\")
            .to_lowercase()
    };
    norm(a) == norm(b)
}

fn is_under(child: &Path, parent: &Path) -> bool {
    let c = child.to_string_lossy().to_lowercase().replace('/', "\\");
    let p = parent.to_string_lossy().to_lowercase().replace('/', "\\");
    let p = p.trim_end_matches('\\');
    c == p || c.starts_with(&format!("{p}\\"))
}

/// 资源管理器在同一文件夹里粘贴时的命名：`名称 - 副本.ext`、`名称 - 副本 (2).ext`。
pub fn copy_name(dst: &Path) -> PathBuf {
    let (stem, ext) = split_name(dst);
    let parent = dst.parent().unwrap_or(Path::new(""));
    let first = parent.join(format!("{stem} - 副本{ext}"));
    if !first.exists() {
        return first;
    }
    (2..)
        .map(|n| parent.join(format!("{stem} - 副本 ({n}){ext}")))
        .find(|p| !p.exists())
        .unwrap()
}

/// 冲突时“两个都保留”的第 n 个候选名：`名称 (n).ext`。
pub fn numbered_name(dst: &Path, n: u32) -> PathBuf {
    let (stem, ext) = split_name(dst);
    dst.with_file_name(format!("{stem} ({n}){ext}"))
}

fn split_name(p: &Path) -> (String, String) {
    let name = p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    // 文件夹和以点开头的文件不拆扩展名
    if p.is_dir() {
        return (name, String::new());
    }
    match name.rfind('.') {
        Some(i) if i > 0 => (name[..i].to_string(), name[i..].to_string()),
        _ => (name, String::new()),
    }
}

#[derive(Default)]
struct Acc {
    dirs: Vec<PathBuf>,
    files: Vec<FileItem>,
    bytes: u64,
    links: Vec<PathBuf>,
    errors: Vec<(PathBuf, String)>,
}

pub fn plan(job: &Job) -> Result<Plan, String> {
    if !job.dest.is_dir() {
        return Err(format!("目标不是文件夹：{}", job.dest.display()));
    }
    let mut out = Plan::default();
    for src in &job.sources {
        let meta = fs::symlink_metadata(src).map_err(|e| format!("{}：{e}", src.display()))?;
        let name = src.file_name().ok_or_else(|| format!("无效路径：{}", src.display()))?;
        let is_dir = meta.is_dir();
        let in_place = src.parent().is_some_and(|p| same_path(p, &job.dest));
        if in_place && job.mode == Mode::Move {
            continue; // 移动到原地，什么都不做
        }
        if is_dir && is_under(&job.dest, src) {
            return Err(format!("目标文件夹在源文件夹里面：{}", src.display()));
        }
        let mut dst = job.dest.join(name);
        if in_place {
            dst = copy_name(&dst);
        }
        let existing = fs::symlink_metadata(&dst).ok();
        let top = TopItem {
            src: src.clone(),
            dst: dst.clone(),
            is_dir,
            merge: is_dir && existing.as_ref().is_some_and(|m| m.is_dir()),
            conflict: existing.as_ref().is_some_and(|m| !m.is_dir() || !is_dir),
        };
        out.tops.push(top);
    }

    let acc = Mutex::new(Acc::default());
    rayon::scope(|s| {
        for (i, t) in out.tops.iter().enumerate() {
            let i = i as u32;
            if t.is_dir && !t.conflict {
                if !t.merge {
                    acc.lock().unwrap().dirs.push(t.dst.clone());
                }
                let (src, dst, merge) = (t.src.clone(), t.dst.clone(), t.merge);
                let acc = &acc;
                s.spawn(move |s| walk(s, src, dst, merge, i, acc));
            } else if !t.is_dir {
                let size = fs::metadata(&t.src).map(|m| m.len()).unwrap_or(0);
                let mut a = acc.lock().unwrap();
                a.bytes += size;
                a.files.push(FileItem { src: t.src.clone(), dst: t.dst.clone(), size, conflict: t.conflict, top: i });
            }
        }
    });
    let a = acc.into_inner().unwrap();
    out.dirs = a.dirs;
    out.files = a.files;
    out.total_bytes = a.bytes;
    out.skipped_links = a.links;
    out.errors = a.errors;
    out.files = interleave(out.files);
    // 大文件排前面，避免最后剩一个大文件单线程拖尾
    out.files.sort_by_key(|f| std::cmp::Reverse(f.size >= crate::BIG_FILE));
    Ok(out)
}

/// 按目录轮流取文件，让并发的线程落在不同目录里，减少抢同一个目录的锁。
fn interleave(files: Vec<FileItem>) -> Vec<FileItem> {
    let mut groups: Vec<std::collections::VecDeque<FileItem>> = Vec::new();
    for f in files {
        match groups.last_mut() {
            Some(g) if g.front().is_some_and(|x| x.dst.parent() == f.dst.parent()) => g.push_back(f),
            _ => groups.push(std::collections::VecDeque::from([f])),
        }
    }
    let total = groups.iter().map(|g| g.len()).sum();
    let mut out = Vec::with_capacity(total);
    while out.len() < total {
        for g in groups.iter_mut() {
            if let Some(f) = g.pop_front() {
                out.push(f);
            }
        }
    }
    out
}

fn walk<'s>(s: &rayon::Scope<'s>, src: PathBuf, dst: PathBuf, check: bool, top: u32, acc: &'s Mutex<Acc>) {
    let rd = match fs::read_dir(&src) {
        Ok(rd) => rd,
        Err(e) => {
            acc.lock().unwrap().errors.push((src, e.to_string()));
            return;
        }
    };
    // 合并到已有文件夹时，一次读出目标目录的名字，代替逐个文件 stat
    let existing: HashSet<String> = if check {
        fs::read_dir(&dst)
            .map(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().to_lowercase()).collect())
            .unwrap_or_default()
    } else {
        HashSet::new()
    };
    let mut local = Acc::default();
    for e in rd {
        let e = match e {
            Ok(e) => e,
            Err(err) => {
                local.errors.push((src.clone(), err.to_string()));
                continue;
            }
        };
        let name = e.file_name();
        let exists = check && existing.contains(&name.to_string_lossy().to_lowercase());
        let (ft, meta) = match (e.file_type(), e.metadata()) {
            (Ok(ft), Ok(m)) => (ft, m),
            (Err(err), _) | (_, Err(err)) => {
                local.errors.push((e.path(), err.to_string()));
                continue;
            }
        };
        let child_dst = dst.join(&name);
        if ft.is_symlink() && meta.is_dir() {
            local.links.push(e.path());
        } else if ft.is_dir() {
            if !exists {
                local.dirs.push(child_dst.clone());
            }
            let p = e.path();
            s.spawn(move |s| walk(s, p, child_dst, exists, top, acc));
        } else {
            let size = meta.len();
            local.bytes += size;
            local.files.push(FileItem { src: e.path(), dst: child_dst, size, conflict: exists, top });
        }
    }
    let mut a = acc.lock().unwrap();
    a.dirs.append(&mut local.dirs);
    a.files.append(&mut local.files);
    a.bytes += local.bytes;
    a.links.append(&mut local.links);
    a.errors.append(&mut local.errors);
}
