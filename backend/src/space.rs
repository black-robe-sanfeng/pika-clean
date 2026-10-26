//! 空间去向：启动盘上的空间都被什么占了。这里只统计；个人文件夹里的个人文件可以移到废纸篓，删除在 `remove::trash_space`（规格切片 18）。

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};

/// 启动盘的数据卷。从 `/` 起数会经过固定链接把同一份数据数两遍，所以只从这里起数。
pub const DATA_ROOT: &str = "/System/Volumes/Data";
/// 正式使用的剪枝标准：每层最多 30 项，小于 100 MB 的不单独记录。
pub const LIMITS: Limits = Limits { top: 30, min_bytes: 100_000_000 };
/// 「读不到」的目录最多记下几个
const MAX_UNREADABLE: usize = 50;
/// 每数这么多个文件看一次要不要报进度
const PROGRESS_EVERY: u64 = 2000;
/// 两次进度之间至少隔这么久（毫秒）。数得再快，界面每秒刷五次也看不出差别
const PROGRESS_MS: u64 = 200;
/// 进度里最多带几个位置
const PROGRESS_TOPS: usize = 6;

#[derive(Clone, Copy)]
pub struct Limits {
    pub top: usize,
    pub min_bytes: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Node {
    pub name: String,
    /// 显示用路径：去掉了数据卷前缀，例如 `/Users/me/Library`
    pub path: String,
    pub size_bytes: u64,
    pub is_dir: bool,
    /// 落在已有清理类别的扫描位置里，界面显示「去清理」
    pub cleanable: bool,
    /// 个人文件夹下的个人文件，可以在这里移到废纸篓（规格切片 18）
    pub trashable: bool,
    /// 按大小降序
    pub children: Vec<Node>,
    /// 没有单独列出的子项加起来的大小。`size_bytes` = 子项之和 + `other_bytes`
    pub other_bytes: u64,
}

/// 存到磁盘上，下次打开直接显示（`db::save_space`）
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SpaceReport {
    pub root: Node,
    /// 被系统隐私保护拦下、授了完全磁盘访问就能读的目录（显示用路径），最多 50 个
    pub unreadable: Vec<String>,
    pub unreadable_count: u32,
    /// 系统需要时会自动腾出的空间，由调用方填
    pub purgeable_bytes: u64,
    /// 中途取消时为 false，数字偏小
    pub finished: bool,
    pub files: u64,
    /// 统计完成的时间（毫秒时间戳）
    #[serde(default)]
    pub scanned_at_ms: u64,
}

#[derive(Serialize, Clone, Debug)]
pub struct SpaceProgress {
    pub files: u64,
    pub bytes: u64,
    /// 目前最大的几个位置，边数边给用户看（按大小降序）
    pub tops: Vec<Top>,
}

/// 统计中的一个位置：个人文件夹下的一级（如 `~/Library`），或启动盘上个人文件夹以外的一级（如 `/Applications`）
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Top {
    pub path: String,
    /// 没数完时是已经数到的大小
    pub bytes: u64,
    pub done: bool,
}

/// `home` 是个人文件夹的显示用路径（如 `/Users/me`），进度按它往下分位置。
pub fn scan(root: &Path, home: &Path, cleanable: &[PathBuf], limits: Limits, cancel: &AtomicBool, on_progress: &(dyn Fn(SpaceProgress) + Sync)) -> SpaceReport {
    let meta = root.symlink_metadata().ok();
    let own = meta.as_ref().map_or(0, |m| m.blocks() * 512);
    let w = Walk {
        root,
        home,
        dev: meta.as_ref().map_or(0, |m| m.dev()),
        limits,
        cancel,
        on_progress,
        queue: Mutex::new(Queue {
            todo: vec![(0, root.to_path_buf())],
            busy: 0,
            pending: HashMap::from([(0, Pending { node: dir_node(String::new(), "/".into(), own), waiting: 0, parent: None, top: None, owns_top: false })]),
            next_id: 1,
            tops: Vec::new(),
            done: None,
        }),
        wake: Condvar::new(),
        linked: Mutex::new(HashSet::new()),
        unreadable: Mutex::new(Vec::new()),
        unreadable_count: AtomicU32::new(0),
        files: AtomicU64::new(0),
        bytes: AtomicU64::new(0),
        started: std::time::Instant::now(),
        last_progress_ms: AtomicU64::new(0),
    };
    // 瓶颈在系统读文件信息，实测 4 个线程以上就不再明显变快，所以最多 4 个；留一个核给界面和别的程序
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).saturating_sub(1).clamp(1, 4);
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| w.work());
        }
    });
    let q = w.queue.into_inner().unwrap();
    let mut tops = q.tops;
    tops.sort_by(|a, b| b.bytes.cmp(&a.bytes));
    tops.truncate(PROGRESS_TOPS);
    let files = w.files.load(Ordering::Relaxed);
    (w.on_progress)(SpaceProgress { files, bytes: w.bytes.load(Ordering::Relaxed), tops });
    let mut tree = q.done.expect("所有文件夹都数完了，根一定整理好了");
    mark_cleanable(&mut tree, cleanable);
    SpaceReport {
        root: tree,
        unreadable: w.unreadable.into_inner().unwrap(),
        unreadable_count: w.unreadable_count.into_inner(),
        purgeable_bytes: 0,
        finished: !cancel.load(Ordering::Relaxed),
        files,
        scanned_at_ms: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64),
    }
}

/// 按显示用路径找树里的节点。前端传来的路径只有在树里才认（规格切片 16）。
pub fn find<'a>(node: &'a Node, path: &str) -> Option<&'a Node> {
    if node.path == path {
        return Some(node);
    }
    node.children.iter().find_map(|c| find(c, path))
}

/// 已有清理类别覆盖的目录。项目依赖、对话记录、重复文件、大文件散在个人文件夹各处，没有固定位置，不算；
/// Homebrew 只有个别旧版本能清，整个 Cellar 也不算。
pub fn cleanable_roots(home: &Path) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = crate::scan::default_rules(home)
        .into_iter()
        .flat_map(|r| {
            if !r.only.is_empty() {
                r.only.iter().map(|o| r.root.join(o)).collect()
            } else if r.root == home || r.id == crate::model::CategoryId::HomebrewOld {
                Vec::new()
            } else {
                vec![r.root]
            }
        })
        .collect();
    // Docker 和模拟器在清理页的「开发环境」里清
    roots.extend(crate::devenv::cleanable_dirs(home));
    roots
}

/// 所有线程共用一个待数的文件夹队列：谁空着谁去领，不会有线程干等。
/// 一个文件夹的子文件夹都数完了，就把它整理好（排序、只留最大的几项）交给上一级。
struct Walk<'a> {
    root: &'a Path,
    home: &'a Path,
    dev: u64,
    limits: Limits,
    cancel: &'a AtomicBool,
    on_progress: &'a (dyn Fn(SpaceProgress) + Sync),
    queue: Mutex<Queue>,
    wake: Condvar,
    /// 链接数大于 1 的文件按（设备号, inode）只算一次
    linked: Mutex<HashSet<(u64, u64)>>,
    unreadable: Mutex<Vec<String>>,
    unreadable_count: AtomicU32,
    files: AtomicU64,
    bytes: AtomicU64,
    started: std::time::Instant,
    /// 上次报进度时距 `started` 的毫秒数
    last_progress_ms: AtomicU64,
}

struct Queue {
    /// 待数的文件夹：（编号, 路径）。后进先出，先往深处数，没数完的文件夹少，省内存
    todo: Vec<(usize, PathBuf)>,
    /// 正在被数的文件夹个数。队列空了且没有人在数，就是全部数完了
    busy: usize,
    /// 已经列出、还在等子文件夹数完的文件夹
    pending: HashMap<usize, Pending>,
    next_id: usize,
    tops: Vec<Top>,
    /// 整理好的根
    done: Option<Node>,
}

struct Pending {
    node: Node,
    /// 还有几个子文件夹没数完
    waiting: usize,
    parent: Option<usize>,
    /// 算进进度里的哪个位置
    top: Option<usize>,
    /// 这个文件夹本身就是那个位置，它数完了那个位置就数完了
    owns_top: bool,
}

/// 列一个文件夹得到的东西
#[derive(Default)]
struct Listed {
    subdirs: Vec<(PathBuf, u64)>,
    big_files: Vec<Node>,
    small_bytes: u64,
    file_bytes: u64,
}

fn dir_node(name: String, path: String, own: u64) -> Node {
    // `own` 是文件夹自己占的块，记进「其他」，保持 size = 子项之和 + other
    Node { name, path, size_bytes: own, is_dir: true, cleanable: false, trashable: false, children: Vec::new(), other_bytes: own }
}

impl Walk<'_> {
    fn display(&self, path: &Path) -> String {
        match path.strip_prefix(self.root) {
            Ok(rest) if rest.as_os_str().is_empty() => "/".to_owned(),
            Ok(rest) => format!("/{}", rest.display()),
            Err(_) => path.display().to_string(),
        }
    }

    fn work(&self) {
        loop {
            let job = {
                let mut q = self.queue.lock().unwrap();
                loop {
                    if let Some(job) = q.todo.pop() {
                        q.busy += 1;
                        break Some(job);
                    }
                    if q.busy == 0 {
                        break None;
                    }
                    q = self.wake.wait(q).unwrap();
                }
            };
            let Some((id, path)) = job else {
                self.wake.notify_all();
                return;
            };
            // 取消后队列里剩下的文件夹不再列，当空文件夹收尾，已经数到的照常交上去
            let listed = if self.cancel.load(Ordering::Relaxed) { Listed::default() } else { self.list(&path) };
            let mut q = self.queue.lock().unwrap();
            let mut kids = Vec::with_capacity(listed.subdirs.len());
            let (parent_display, top) = {
                let p = q.pending.get_mut(&id).expect("刚领的文件夹一定在等");
                p.node.other_bytes += listed.small_bytes;
                p.node.children.extend(listed.big_files);
                p.waiting = listed.subdirs.len();
                (p.node.path.clone(), p.top)
            };
            if let Some(t) = top {
                q.tops[t].bytes += listed.file_bytes;
            }
            for (sub, own) in listed.subdirs {
                let display = self.display(&sub);
                let (top, owns_top) = match top {
                    Some(t) => (Some(t), false),
                    None if self.is_top(&parent_display, &display) => {
                        q.tops.push(Top { path: display.clone(), bytes: own, done: false });
                        (Some(q.tops.len() - 1), true)
                    }
                    None => (None, false),
                };
                if let (Some(t), false) = (top, owns_top) {
                    q.tops[t].bytes += own;
                }
                let child = q.next_id;
                q.next_id += 1;
                q.pending.insert(child, Pending { node: dir_node(name_of(&sub), display, own), waiting: 0, parent: Some(id), top, owns_top });
                kids.push((child, sub));
            }
            if kids.is_empty() {
                self.finish(&mut q, id);
            }
            q.todo.extend(kids);
            q.busy -= 1;
            drop(q);
            self.wake.notify_all();
        }
    }

    /// 进度按位置分：个人文件夹下的一级，或启动盘上个人文件夹以外的一级
    fn is_top(&self, parent: &str, path: &str) -> bool {
        parent == self.home.to_string_lossy() || (parent == "/" && !self.home.starts_with(path))
    }

    /// 列出一个文件夹：子文件夹交回队列，文件当场算大小
    fn list(&self, path: &Path) -> Listed {
        let mut out = Listed::default();
        let entries = match fs::read_dir(path) {
            Ok(entries) => entries,
            Err(e) => {
                if needs_access(&e) {
                    self.unreadable_count.fetch_add(1, Ordering::Relaxed);
                    let mut list = self.unreadable.lock().unwrap();
                    if list.len() < MAX_UNREADABLE {
                        list.push(self.display(path));
                    }
                }
                return out;
            }
        };
        for entry in entries.flatten() {
            if self.cancel.load(Ordering::Relaxed) {
                break;
            }
            let p = entry.path();
            // 不跟随符号链接，不进入别的磁盘
            let Ok(meta) = p.symlink_metadata() else { continue };
            if meta.dev() != self.dev {
                continue;
            }
            if meta.is_dir() {
                out.subdirs.push((p, meta.blocks() * 512));
            } else {
                let size = self.file(&meta);
                out.file_bytes += size;
                // 小文件不建节点：数百万个文件时省内存
                if size >= self.limits.min_bytes {
                    out.big_files.push(Node { name: name_of(&p), path: self.display(&p), size_bytes: size, is_dir: false, cleanable: false, trashable: false, children: Vec::new(), other_bytes: 0 });
                } else {
                    out.small_bytes += size;
                }
            }
        }
        out
    }

    /// 这个文件夹的子文件夹都数完了：整理好交给上一级；上一级因此也齐了就接着往上交
    fn finish(&self, q: &mut Queue, mut id: usize) {
        loop {
            let p = q.pending.remove(&id).expect("要收尾的文件夹一定在等");
            let mut node = p.node;
            let mut kids = std::mem::take(&mut node.children);
            node.size_bytes = kids.iter().map(|k| k.size_bytes).sum::<u64>() + node.other_bytes;
            kids.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes));
            let keep = kids.iter().take(self.limits.top).take_while(|k| k.size_bytes >= self.limits.min_bytes).count();
            node.other_bytes += kids[keep..].iter().map(|k| k.size_bytes).sum::<u64>();
            kids.truncate(keep);
            node.children = kids;
            if let (Some(t), true) = (p.top, p.owns_top) {
                q.tops[t] = Top { path: node.path.clone(), bytes: node.size_bytes, done: true };
            }
            let Some(parent) = p.parent else {
                q.done = Some(node);
                return;
            };
            let up = q.pending.get_mut(&parent).expect("上一级还在等");
            up.node.children.push(node);
            up.waiting -= 1;
            if up.waiting > 0 {
                return;
            }
            id = parent;
        }
    }

    fn file(&self, meta: &fs::Metadata) -> u64 {
        let first = meta.nlink() <= 1 || self.linked.lock().unwrap().insert((meta.dev(), meta.ino()));
        let size = if first { meta.blocks() * 512 } else { 0 };
        let files = self.files.fetch_add(1, Ordering::Relaxed) + 1;
        let bytes = self.bytes.fetch_add(size, Ordering::Relaxed) + size;
        if files % PROGRESS_EVERY == 0 && due(&self.last_progress_ms, self.started.elapsed().as_millis() as u64) {
            let mut tops = self.queue.lock().unwrap().tops.clone();
            tops.sort_by(|a, b| b.bytes.cmp(&a.bytes));
            tops.truncate(PROGRESS_TOPS);
            (self.on_progress)(SpaceProgress { files, bytes, tops });
        }
        size
    }
}

/// 距上次报进度满 `PROGRESS_MS` 才放行；几个线程同时到时只放行一个
fn due(last: &AtomicU64, now_ms: u64) -> bool {
    let prev = last.load(Ordering::Relaxed);
    now_ms >= prev + PROGRESS_MS && last.compare_exchange(prev, now_ms, Ordering::Relaxed, Ordering::Relaxed).is_ok()
}

/// 取消时手里已有一份数完的结果：留着它，不拿没数完的覆盖。第一次统计就取消时没有可留的，照常给出数到的部分
pub fn keep_previous(new: &SpaceReport, prev: Option<&SpaceReport>) -> bool {
    !new.finished && prev.is_some_and(|p| p.finished)
}

/// 个人文件夹下这几处不能在空间去向里删：资源库里是应用的数据和设置，应用在卸载页删，其余是废纸篓和密钥
const KEEP_IN_HOME: [&str; 5] = ["Library", "Applications", ".Trash", ".ssh", ".gnupg"];

/// 标出能移到废纸篓的项：在个人文件夹里面（不含它本身）、不在 `KEEP_IN_HOME` 下、不属于已有清理类别（那些去智能扫描清）。
/// `home` 是显示用路径，例如 `/Users/me`
pub fn mark_trashable(node: &mut Node, home: &Path) {
    let p = Path::new(&node.path);
    let first = p.strip_prefix(home).ok().and_then(|rest| rest.components().next());
    node.trashable = !node.cleanable && first.is_some_and(|c| matches!(c, Component::Normal(n) if !n.to_str().is_some_and(|n| KEEP_IN_HOME.contains(&n))));
    for c in &mut node.children {
        mark_trashable(c, home);
    }
}

/// 移到废纸篓之后改统计结果，不用重新统计：这一项从原处拿掉，大小记到 `trash`（个人文件夹的 `.Trash`）下面，
/// 因为清空废纸篓之前空间并没有释放。树里没有 `trash` 时记到离它最近的上级的「其他」里。返回挪走的大小
pub fn move_to_trash(root: &mut Node, path: &str, trash: &str) -> Option<u64> {
    let size = detach(root, path)?;
    add(root, trash, size);
    Some(size)
}

fn add(node: &mut Node, path: &str, size: u64) {
    node.size_bytes += size;
    match node.children.iter_mut().find(|c| Path::new(path).starts_with(&c.path)) {
        Some(c) => add(c, path, size),
        None => node.other_bytes += size,
    }
    node.children.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes));
}

fn detach(node: &mut Node, path: &str) -> Option<u64> {
    let size = match node.children.iter().position(|c| c.path == path) {
        Some(i) => node.children.remove(i).size_bytes,
        None => detach(node.children.iter_mut().find(|c| Path::new(path).starts_with(&c.path))?, path)?,
    };
    node.size_bytes -= size;
    node.children.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes));
    Some(size)
}

fn mark_cleanable(node: &mut Node, roots: &[PathBuf]) {
    node.cleanable = roots.iter().any(|r| Path::new(&node.path).starts_with(r));
    for c in &mut node.children {
        mark_cleanable(c, roots);
    }
}

/// 系统隐私保护拦下的读取是 EPERM，授了完全磁盘访问就能读。
/// 文件本身的权限不够（EACCES，例如只有 root 能读的目录）授权了也读不到，不算进「读不到」，免得一直让用户去授权。
fn needs_access(e: &std::io::Error) -> bool {
    e.raw_os_error() == Some(libc::EPERM)
}

fn name_of(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::testutil::TempTree;
    use std::os::unix::fs::PermissionsExt;
    use std::time::Instant;

    // 测试用小门槛：每层留 2 项，32 KB 以下不单独记录
    const SMALL: Limits = Limits { top: 2, min_bytes: 32 * 1024 };
    const KB64: usize = 64 * 1024;

    fn run(root: &Path, cleanable: &[PathBuf]) -> SpaceReport {
        scan(root, Path::new("/Users/me"), cleanable, SMALL, &AtomicBool::new(false), &|_| {})
    }
    fn sum(n: &Node) -> u64 {
        n.children.iter().map(|c| c.size_bytes).sum::<u64>() + n.other_bytes
    }

    #[test]
    fn 自底向上累加且每层只留最大的几项() {
        let t = TempTree::new();
        t.file("a/big1", KB64 * 3);
        t.file("a/big2", KB64 * 2);
        t.file("a/big3", KB64); // 第 3 大，每层只留 2 项，进「其他」
        t.file("a/tiny", 1); // 小于门槛，进「其他」
        t.file("b/x", KB64);
        let r = run(&t.0, &[]);
        assert!(r.finished);
        assert_eq!(r.root.path, "/");
        let a = r.root.children.iter().find(|c| c.name == "a").unwrap();
        assert_eq!(a.path, "/a");
        assert!(a.is_dir);
        assert_eq!(a.children.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["big1", "big2"]);
        assert_eq!(a.children[0].path, "/a/big1");
        assert!(!a.children[0].is_dir);
        assert!(a.other_bytes >= (KB64 + 1) as u64, "第三大的文件和小文件都进了其他");
        for n in [&r.root, a] {
            assert_eq!(n.size_bytes, sum(n), "大小等于子项加其他：{}", n.path);
        }
        assert!(r.root.size_bytes >= (KB64 * 7) as u64);
    }

    #[test]
    fn 多线程统计不丢不重() {
        let t = TempTree::new();
        for i in 0..40 {
            for j in 0..5 {
                t.file(&format!("d{i}/e{j}/f"), KB64);
            }
        }
        let r = run(&t.0, &[]);
        assert_eq!(r.files, 200);
        assert_eq!(r.root.size_bytes, sum(&r.root));
        assert!(r.root.size_bytes >= (KB64 * 200) as u64);
    }

    #[test]
    fn 硬链接只算一次_符号链接不跟随() {
        let t = TempTree::new();
        let f = t.file("a/f", KB64 * 2);
        fs::hard_link(&f, t.0.join("a/g")).unwrap();
        std::os::unix::fs::symlink(t.0.join("a"), t.0.join("link")).unwrap();
        let r = run(&t.0, &[]);
        let a = r.root.children.iter().find(|c| c.name == "a").unwrap();
        assert_eq!(a.children.len(), 1, "两个硬链接只有一个算了大小");
        assert!(a.size_bytes < (KB64 * 4) as u64);
        assert!(r.root.children.iter().all(|c| c.name != "link"), "符号链接不当文件夹数");
        assert!(r.root.size_bytes < (KB64 * 4) as u64, "没顺着符号链接把 a 再数一遍");
    }

    #[test]
    fn 进度按位置分_个人文件夹往下多分一层() {
        let t = TempTree::new();
        t.file("Users/me/code/a/f", KB64 * 3);
        t.file("Users/me/Library/f", KB64 * 2);
        t.file("Users/me/top-file", KB64);
        t.file("Users/other/f", KB64);
        t.file("Applications/A.app/f", KB64);
        let last = Mutex::new(None);
        let r = scan(&t.0, Path::new("/Users/me"), &[], SMALL, &AtomicBool::new(false), &|p| *last.lock().unwrap() = Some(p));
        let p = last.into_inner().unwrap().expect("数完时报一次进度");
        assert_eq!(p.files, r.files);
        let paths: Vec<&str> = p.tops.iter().map(|t| t.path.as_str()).collect();
        assert_eq!(paths, ["/Users/me/code", "/Users/me/Library", "/Applications"], "按大小降序；/Users、个人文件夹本身和别人的文件夹不单列");
        assert!(p.tops.iter().all(|t| t.done));
        assert_eq!(p.tops[0].bytes, find(&r.root, "/Users/me/code").unwrap().size_bytes, "数完的位置大小和结果一致");
    }

    #[test]
    fn 取消后标为没统计完() {
        let t = TempTree::new();
        t.file("a/f", KB64);
        let r = scan(&t.0, Path::new("/Users/me"), &[], SMALL, &AtomicBool::new(true), &|_| {});
        assert!(!r.finished);
        assert!(r.root.children.is_empty());
        // 已有数完的结果时取消，留旧的；第一次就取消、或旧的也没数完，用新的
        let done = run(&t.0, &[]);
        assert!(keep_previous(&r, Some(&done)));
        assert!(!keep_previous(&r, None));
        assert!(!keep_previous(&r, Some(&r)));
        assert!(!keep_previous(&done, Some(&done)));
    }

    #[test]
    fn 进度按时间节流_同时到的只放行一个() {
        let last = AtomicU64::new(0);
        assert!(!due(&last, PROGRESS_MS - 1));
        assert!(due(&last, PROGRESS_MS));
        assert!(!due(&last, PROGRESS_MS), "同一时刻第二个线程不放行");
        assert!(!due(&last, PROGRESS_MS * 2 - 1));
        assert!(due(&last, PROGRESS_MS * 2));
    }

    #[test]
    fn 只有授权能解决的才算读不到() {
        // 系统隐私保护拦下的是 EPERM，授了完全磁盘访问就能读；文件本身的权限（EACCES，例如只有 root 能读）授权了也读不到
        assert!(needs_access(&std::io::Error::from_raw_os_error(libc::EPERM)));
        assert!(!needs_access(&std::io::Error::from_raw_os_error(libc::EACCES)));
        let t = TempTree::new();
        t.file("locked/f", 1);
        let locked = t.0.join("locked");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let r = run(&t.0, &[]);
        // 先恢复权限，TempTree 才删得掉
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!((r.unreadable_count, r.unreadable.len()), (0, 0), "chmod 000 是 EACCES，不该让用户去授权");
    }

    #[test]
    fn 落在清理类别里的标为可清理_查找只认树里的路径() {
        let t = TempTree::new();
        t.file("Library/Caches/app/f", KB64);
        t.file("Documents/f", KB64);
        let r = run(&t.0, &[PathBuf::from("/Library/Caches")]);
        assert!(!find(&r.root, "/Library").unwrap().cleanable, "上一级不算");
        assert!(find(&r.root, "/Library/Caches").unwrap().cleanable);
        assert!(find(&r.root, "/Library/Caches/app").unwrap().cleanable, "下面的也算");
        assert!(!find(&r.root, "/Documents").unwrap().cleanable);
        assert!(find(&r.root, "/Library/Caches/../Caches").is_none());
        assert!(find(&r.root, "/etc").is_none());
    }

    #[test]
    fn 清理类别的位置() {
        let home = Path::new("/Users/me");
        let roots = cleanable_roots(home);
        assert!(roots.contains(&home.join("Library/Caches")));
        assert!(roots.contains(&home.join(".Trash")));
        assert!(roots.contains(&home.join(crate::scan::DEV_CACHE_DIRS[0])), "固定路径的类别按每个路径算");
        assert!(!roots.contains(&home.to_path_buf()), "个人文件夹本身不算，不然处处都能去清理");
        assert!(roots.contains(&home.join("Library/Containers/com.docker.docker")));
        assert!(roots.contains(&PathBuf::from("/cores")), "崩溃转储");
        assert!(!roots.iter().any(|r| r.ends_with("Cellar")), "Homebrew 只有个别旧版本能清");
    }

    #[test]
    fn 个人文件夹里的个人文件才能移到废纸篓() {
        let t = TempTree::new();
        for f in ["Users/me/huixuan/big", "Users/me/.m2/big", "Users/me/Library/Caches/big", "Users/me/Library/Mail/big", "Users/me/Applications/A.app/big", "Users/me/.ssh/big", "Users/me/.Trash/big", "Users/me/Downloads/big", "Users/other/big", "Applications/B.app/big", "opt/big"] {
            t.file(f, KB64);
        }
        let cleanable = [PathBuf::from("/Users/me/Library/Caches"), PathBuf::from("/Users/me/Downloads")];
        let mut r = scan(&t.0, Path::new("/Users/me"), &cleanable, Limits { top: 30, min_bytes: 1 }, &AtomicBool::new(false), &|_| {});
        mark_trashable(&mut r.root, Path::new("/Users/me"));
        let can = |p: &str| find(&r.root, p).unwrap_or_else(|| panic!("树里没有 {p}")).trashable;
        assert!(can("/Users/me/huixuan") && can("/Users/me/huixuan/big"), "个人文件夹下的文件夹和文件");
        assert!(can("/Users/me/.m2"), "不属于清理类别的点开头文件夹也算个人文件");
        assert!(!can("/Users/me"), "个人文件夹本身不能删");
        for p in ["/Users/me/Library", "/Users/me/Library/Mail", "/Users/me/Applications", "/Users/me/.ssh", "/Users/me/.Trash"] {
            assert!(!can(p), "{p} 不能在这里删");
        }
        assert!(!can("/Users/me/Downloads"), "属于清理类别的去智能扫描清");
        for p in ["/Users", "/Users/other", "/Applications", "/Applications/B.app", "/opt"] {
            assert!(!can(p), "个人文件夹以外的 {p} 不能删");
        }
    }

    #[test]
    fn 移到废纸篓后大小记到废纸篓下() {
        let t = TempTree::new();
        t.file("me/a/b/big", KB64 * 3);
        t.file("me/a/c/big", KB64 * 2);
        t.file("me/.Trash/old", KB64);
        let mut r = scan(&t.0, Path::new("/Users/me"), &[], Limits { top: 30, min_bytes: 1 }, &AtomicBool::new(false), &|_| {});
        let size = |r: &SpaceReport, p: &str| find(&r.root, p).unwrap().size_bytes;
        let (root, me, a, b, trash) = (r.root.size_bytes, size(&r, "/me"), size(&r, "/me/a"), size(&r, "/me/a/b"), size(&r, "/me/.Trash"));
        assert_eq!(move_to_trash(&mut r.root, "/me/a/b", "/me/.Trash"), Some(b));
        assert!(find(&r.root, "/me/a/b").is_none());
        assert_eq!(size(&r, "/me/a"), a - b);
        assert_eq!(size(&r, "/me/.Trash"), trash + b);
        assert_eq!((r.root.size_bytes, size(&r, "/me")), (root, me), "空间还没释放，上级总数不变");
        for p in ["/", "/me", "/me/a", "/me/.Trash"] {
            let n = find(&r.root, p).unwrap();
            assert_eq!(n.size_bytes, sum(n), "大小等于子项加其他：{p}");
        }
        assert_eq!(move_to_trash(&mut r.root, "/me/a/b", "/me/.Trash"), None, "不在树里的不动");
        // 树里没有废纸篓时记到最近的上级的「其他」里
        let c = size(&r, "/me/a/c");
        move_to_trash(&mut r.root, "/me/a/c", "/nope/.Trash").unwrap();
        assert_eq!(r.root.size_bytes, root);
        assert_eq!(r.root.size_bytes, sum(&r.root));
        assert!(r.root.other_bytes >= c);
    }

    /// 本机真实统计一遍：`cargo test --lib 在本机真实统计 -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn 在本机真实统计一遍() {
        let start = Instant::now();
        let home = PathBuf::from(std::env::var_os("HOME").unwrap());
        let r = scan(Path::new(DATA_ROOT), &home, &[], LIMITS, &AtomicBool::new(false), &|_| {});
        eprintln!("用时 {:?}，{} 个文件，合计 {} 字节，读不到 {} 个", start.elapsed(), r.files, r.root.size_bytes, r.unreadable_count);
        for c in &r.root.children {
            eprintln!("{:>9} MB  {}", c.size_bytes / 1_000_000, c.path);
        }
    }
}
