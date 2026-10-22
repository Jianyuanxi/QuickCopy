//! 扫描阶段：把要复制/移动的东西展开成目录清单和文件清单，顺带算总量、找冲突。

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mode {
    #[default]
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
    /// 只在改名计划里出现：整个文件夹一次改名过去。
    pub is_dir: bool,
}

#[derive(Debug, Default)]
pub struct Plan {
    /// Move 且不是改名计划时：复制完核对大小，再删源。
    pub mode: Mode,
    pub tops: Vec<TopItem>,
    /// 需要新建的目标目录。
    pub dirs: Vec<PathBuf>,
    /// 复制计划：要复制的文件；改名计划：要改名的文件或整个文件夹。
    pub files: Vec<FileItem>,
    pub total_bytes: u64,
    /// 移动完成后要删掉的源文件夹（空了才删），深的在前。
    pub src_dirs: Vec<PathBuf>,
    /// 同盘移动：直接改名，不复制。
    pub rename: bool,
    /// 没有展开的目录符号链接 / 联接点。
    pub skipped_links: Vec<PathBuf>,
    pub errors: Vec<(PathBuf, String)>,
}

/// 能重新生成的依赖、缓存目录（.git 例外，是版本历史），复制时问要不要跳过。
pub const DEP_DIRS: [&str; 5] = ["node_modules", ".venv", "venv", "__pycache__", ".git"];
/// 依赖目录里的文件加起来少于这么多就不问：跳过省不了几秒，弹窗反而打断。
pub const DEP_ASK_MIN_FILES: usize = 300;

fn is_dep(name: &std::ffi::OsStr, git: bool) -> bool {
    let n = name.to_string_lossy();
    DEP_DIRS.iter().any(|d| n.eq_ignore_ascii_case(d)) && (git || !n.eq_ignore_ascii_case(".git"))
}

/// 对依赖目录的处理。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DepSkip {
    /// 全部复制。
    None,
    /// 跳过能重新生成的，.git 照常复制。
    KeepGit,
    /// 连 .git 一起跳过。
    All,
}

/// 选中的文件夹里面的一个依赖目录（嵌套的只算最外层）。
#[derive(Clone, Debug)]
pub struct DepDir {
    /// 相对顶层条目所在文件夹的路径，如 `项目\node_modules`。
    pub rel: PathBuf,
    pub files: usize,
    pub bytes: u64,
}

impl Plan {
    pub fn conflict_count(&self) -> usize {
        self.files.iter().filter(|f| f.conflict).count()
    }

    /// `path` 在第 `top` 个顶层条目（根是 `root`）里面时，它所在的最外层依赖目录，相对 `root`。
    /// 顶层条目本身叫 node_modules 不算：那是用户点名要拷的。
    /// `git` 为 false 时不把 .git 当依赖目录。
    fn dep_root(&self, path: &Path, top: u32, dst: bool, git: bool) -> Option<PathBuf> {
        let t = self.tops.get(top as usize)?;
        let root = if dst { &t.dst } else { &t.src };
        let rel = path.strip_prefix(root).ok()?;
        let mut acc = PathBuf::new();
        for c in rel.components() {
            acc.push(c);
            if is_dep(c.as_os_str(), git) {
                return Some(acc);
            }
        }
        None
    }

    /// 计划里的依赖目录，按位置排序。
    pub fn dep_dirs(&self) -> Vec<DepDir> {
        let mut map: std::collections::BTreeMap<PathBuf, (usize, u64)> = Default::default();
        for f in &self.files {
            // 只看文件所在的目录，文件本身叫 venv 不算
            if let Some(d) = f.src.parent().and_then(|p| self.dep_root(p, f.top, false, true)) {
                let top_name = self.tops[f.top as usize].src.file_name().map(PathBuf::from).unwrap_or_default();
                let e = map.entry(top_name.join(d)).or_default();
                e.0 += 1;
                e.1 += f.size;
            }
        }
        map.into_iter().map(|(rel, (files, bytes))| DepDir { rel, files, bytes }).collect()
    }

    /// 从计划里去掉依赖目录里的文件和要建的目录，返回去掉的文件数。只用于复制计划。
    pub fn drop_deps(&mut self, how: DepSkip) -> usize {
        if how == DepSkip::None {
            return 0;
        }
        let git = how == DepSkip::All;
        let before = self.files.len();
        let keep: Vec<bool> = self.files.iter().map(|f| f.src.parent().and_then(|p| self.dep_root(p, f.top, false, git)).is_none()).collect();
        let mut it = keep.iter();
        self.files.retain(|_| *it.next().unwrap());
        self.total_bytes = self.files.iter().map(|f| f.size).sum();
        let tops: Vec<u32> = (0..self.tops.len() as u32).collect();
        let keep_dir: Vec<bool> = self.dirs.iter().map(|d| tops.iter().all(|&t| self.dep_root(d, t, true, git).is_none())).collect();
        let mut it = keep_dir.iter();
        self.dirs.retain(|_| *it.next().unwrap());
        before - self.files.len()
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

/// 一个顶层条目从 `src` 到 `dst`。源和目标一个是文件一个是文件夹时没法合并也不能替换，返回错误。
pub fn top_item(src: &Path, dst: &Path) -> Result<TopItem, String> {
    let meta = fs::symlink_metadata(src).map_err(|e| e.to_string())?;
    let is_dir = meta.is_dir();
    let existing = fs::symlink_metadata(dst).ok();
    if let Some(m) = &existing {
        if m.is_dir() != is_dir {
            return Err(if is_dir { "目标位置已有同名文件，没法放文件夹".into() } else { "目标位置已有同名文件夹，没法放文件".into() });
        }
    }
    Ok(TopItem {
        src: src.to_path_buf(),
        dst: dst.to_path_buf(),
        is_dir,
        merge: is_dir && existing.is_some(),
        conflict: !is_dir && existing.is_some(),
    })
}

/// 把任务展开成顶层条目。原地移动的条目直接略过，原地复制按资源管理器的规则起“ - 副本”名。
fn build_tops(job: &Job) -> Result<(Vec<TopItem>, Vec<(PathBuf, String)>), String> {
    if !job.dest.is_dir() {
        return Err(format!("目标不是文件夹：{}", job.dest.display()));
    }
    let (mut tops, mut errors) = (Vec::new(), Vec::new());
    for src in &job.sources {
        let name = src.file_name().ok_or_else(|| format!("无效路径：{}", src.display()))?;
        let in_place = src.parent().is_some_and(|p| same_path(p, &job.dest));
        if in_place && job.mode == Mode::Move {
            continue; // 移动到原地，什么都不做
        }
        if src.is_dir() && is_under(&job.dest, src) {
            return Err(format!("目标文件夹在源文件夹里面：{}", src.display()));
        }
        let mut dst = job.dest.join(name);
        if in_place {
            dst = copy_name(&dst);
        }
        match top_item(src, &dst) {
            Ok(t) => tops.push(t),
            Err(e) => errors.push((src.clone(), e)),
        }
    }
    Ok((tops, errors))
}

#[derive(Default)]
struct Acc {
    dirs: Vec<PathBuf>,
    src_dirs: Vec<PathBuf>,
    files: Vec<FileItem>,
    bytes: u64,
    links: Vec<PathBuf>,
    errors: Vec<(PathBuf, String)>,
}

/// 复制计划（跨盘移动也用它：先复制，再删源）。
pub fn plan(job: &Job) -> Result<Plan, String> {
    let (tops, errors) = build_tops(job)?;
    let mut p = expand(tops);
    p.mode = job.mode;
    p.errors.splice(0..0, errors);
    Ok(p)
}

/// 按任务选计划：移动且源和目标在同一个卷上就改名，否则复制（移动的再删源）。
pub fn prepare(job: &Job) -> Result<Plan, String> {
    if job.mode == Mode::Move && job.sources.iter().all(|s| crate::win::same_volume(s, &job.dest)) {
        rename_plan(job)
    } else {
        plan(job)
    }
}

/// 把顶层条目展开成要建的目录和要复制的文件。
pub fn expand(tops: Vec<TopItem>) -> Plan {
    let mut out = Plan { tops, ..Default::default() };
    let acc = Mutex::new(Acc::default());
    rayon::scope(|s| {
        for (i, t) in out.tops.iter().enumerate() {
            let i = i as u32;
            if t.is_dir {
                if !t.merge {
                    acc.lock().unwrap().dirs.push(t.dst.clone());
                }
                let (src, dst, merge) = (t.src.clone(), t.dst.clone(), t.merge);
                let acc = &acc;
                s.spawn(move |s| walk(s, src, dst, merge, i, acc));
            } else {
                let size = fs::metadata(&t.src).map(|m| m.len()).unwrap_or(0);
                let mut a = acc.lock().unwrap();
                a.bytes += size;
                a.files.push(FileItem { src: t.src.clone(), dst: t.dst.clone(), size, conflict: t.conflict, top: i, is_dir: false });
            }
        }
    });
    let a = acc.into_inner().unwrap();
    out.dirs = a.dirs;
    out.src_dirs = a.src_dirs;
    out.src_dirs.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
    out.files = a.files;
    out.total_bytes = a.bytes;
    out.skipped_links = a.links;
    out.errors = a.errors;
    out.files = interleave(out.files);
    // 大文件排前面，避免最后剩一个大文件单线程拖尾
    out.files.sort_by_key(|f| std::cmp::Reverse(f.size >= crate::BIG_FILE));
    out
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
            if exists && !child_dst.is_dir() {
                local.errors.push((e.path(), "目标位置已有同名文件，没法放文件夹".into()));
                continue;
            }
            if !exists {
                local.dirs.push(child_dst.clone());
            }
            let p = e.path();
            s.spawn(move |s| walk(s, p, child_dst, exists, top, acc));
        } else {
            if exists && child_dst.is_dir() {
                local.errors.push((e.path(), "目标位置已有同名文件夹，没法放文件".into()));
                continue;
            }
            let size = meta.len();
            local.bytes += size;
            local.files.push(FileItem { src: e.path(), dst: child_dst, size, conflict: exists, top, is_dir: false });
        }
    }
    let mut a = acc.lock().unwrap();
    a.src_dirs.push(src);
    a.dirs.append(&mut local.dirs);
    a.files.append(&mut local.files);
    a.bytes += local.bytes;
    a.links.append(&mut local.links);
    a.errors.append(&mut local.errors);
}

/// 同盘移动的计划：目标没有同名的就整个改名过去（文件夹也是一次改名），
/// 只有合并进已有文件夹时才往里走，逐个处理。
pub fn rename_plan(job: &Job) -> Result<Plan, String> {
    let (tops, errors) = build_tops(job)?;
    let mut out = Plan { mode: Mode::Move, rename: true, errors, ..Default::default() };
    for (i, t) in tops.iter().enumerate() {
        let i = i as u32;
        if t.merge {
            rename_walk(&t.src, &t.dst, i, &mut out);
        } else {
            let size = if t.is_dir { 0 } else { fs::metadata(&t.src).map(|m| m.len()).unwrap_or(0) };
            out.total_bytes += size;
            out.files.push(FileItem { src: t.src.clone(), dst: t.dst.clone(), size, conflict: t.conflict, top: i, is_dir: t.is_dir });
        }
    }
    out.tops = tops;
    Ok(out)
}

fn rename_walk(src: &Path, dst: &Path, top: u32, out: &mut Plan) {
    let rd = match fs::read_dir(src) {
        Ok(rd) => rd,
        Err(e) => return out.errors.push((src.to_path_buf(), e.to_string())),
    };
    for e in rd.flatten() {
        let child_dst = dst.join(e.file_name());
        match top_item(&e.path(), &child_dst) {
            Ok(t) if t.merge => rename_walk(&t.src, &t.dst, top, out),
            Ok(t) => {
                let size = if t.is_dir { 0 } else { e.metadata().map(|m| m.len()).unwrap_or(0) };
                out.total_bytes += size;
                out.files.push(FileItem { src: t.src, dst: t.dst, size, conflict: t.conflict, top, is_dir: t.is_dir });
            }
            Err(err) => out.errors.push((e.path(), err)),
        }
    }
    out.src_dirs.push(src.to_path_buf()); // 后序：子目录先进
}
