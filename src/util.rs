//! 通用工具：目录大小统计、路径规范化、时间格式化等。
//! 所有函数均为只读操作，且对错误完全容错（拿不到就返回默认值，绝不 panic）。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::UNIX_EPOCH;
use walkdir::WalkDir;

#[derive(Debug, Default, Clone, Copy)]
pub struct DirStat {
    pub bytes: u64,
    pub files: u64,
    pub max_mtime_epoch: u64,
}

/// 递归统计目录大小（字节）、文件数、最新修改时间。符号链接/目录联接不跟随。
pub fn dir_stat(path: &Path) -> DirStat {
    let mut st = DirStat::default();
    for entry in WalkDir::new(path).follow_links(false).into_iter().flatten() {
        if entry.file_type().is_file() {
            if let Ok(meta) = entry.metadata() {
                st.bytes += meta.len();
                st.files += 1;
                if let Ok(mtime) = meta.modified() {
                    if let Ok(dur) = mtime.duration_since(UNIX_EPOCH) {
                        st.max_mtime_epoch = st.max_mtime_epoch.max(dur.as_secs());
                    }
                }
            }
        }
    }
    st
}

/// 在目录内（限深度）查找指定文件名的所有出现位置。不跟随链接。
pub fn find_files_named(path: &Path, name_lower: &str, max_depth: usize, cap: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in WalkDir::new(path)
        .follow_links(false)
        .max_depth(max_depth)
        .into_iter()
        .flatten()
    {
        if entry.file_type().is_file() {
            if let Some(n) = entry.file_name().to_str() {
                if n.to_lowercase() == name_lower {
                    out.push(entry.path().to_path_buf());
                    if out.len() >= cap {
                        break;
                    }
                }
            }
        }
    }
    out
}

/// 路径规范化（用于覆盖集合比对）：小写 + 统一反斜杠 + 去尾部分隔符。
pub fn normalize_path(p: &Path) -> String {
    let s = p.to_string_lossy().replace('/', "\\");
    let s = s.trim_end_matches('\\').to_lowercase();
    s
}

pub fn human_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    const TB: f64 = GB * 1024.0;
    let b = bytes as f64;
    if b >= TB {
        format!("{:.2} TB", b / TB)
    } else if b >= GB {
        format!("{:.2} GB", b / GB)
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{} B", bytes)
    }
}

pub fn fmt_epoch(epoch: u64) -> String {
    if epoch == 0 {
        return "-".to_string();
    }
    match chrono::DateTime::from_timestamp(epoch as i64, 0) {
        Some(dt) => dt
            .with_timezone(&chrono::Local)
            .format("%Y-%m-%d %H:%M")
            .to_string(),
        None => format!("{}", epoch),
    }
}

pub fn env_path(var: &str) -> Option<PathBuf> {
    std::env::var_os(var).map(PathBuf::from)
}

pub fn user_profile() -> Option<PathBuf> {
    env_path("USERPROFILE")
}

// ---------- 目录大小缓存（增量扫描 + 存量快速验证） ----------
// 指纹 = 目录自身 mtime + 顶层子项数（各一次 syscall，秒级验证全盘缓存）。
// 游戏残留目录在卸载后基本静止，指纹变化即重算，否则直接复用上次结果。

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SizeEntry {
    bytes: u64,
    files: u64,
    max_mtime_epoch: u64,
    dir_mtime_epoch: u64,
    children: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct SizeCacheFile {
    entries: HashMap<String, SizeEntry>,
}

static SIZE_CACHE: OnceLock<Mutex<SizeCacheFile>> = OnceLock::new();
static SIZE_CACHE_DIRTY: AtomicBool = AtomicBool::new(false);

fn size_cache_path() -> Option<PathBuf> {
    env_path("LOCALAPPDATA").map(|p| p.join("SteamSweep").join("size-cache.json"))
}

fn size_cache() -> &'static Mutex<SizeCacheFile> {
    SIZE_CACHE.get_or_init(|| {
        let loaded = size_cache_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Mutex::new(loaded)
    })
}

/// 目录指纹：（目录 mtime 秒，顶层子项数）。失败返回 None（调用方退化为实时统计）。
fn dir_fingerprint(path: &Path) -> Option<(u64, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_secs();
    let children = std::fs::read_dir(path).ok()?.count() as u64;
    Some((mtime, children))
}

/// dir_stat 的缓存版：指纹未变直接复用，变化则实算并写回缓存。
pub fn dir_stat_cached(path: &Path) -> DirStat {
    let fp = match dir_fingerprint(path) {
        Some(f) => f,
        None => return dir_stat(path),
    };
    let key = normalize_path(path);
    {
        let cache = match size_cache().lock() {
            Ok(c) => c,
            Err(_) => return dir_stat(path),
        };
        if let Some(e) = cache.entries.get(&key) {
            if e.dir_mtime_epoch == fp.0 && e.children == fp.1 {
                return DirStat {
                    bytes: e.bytes,
                    files: e.files,
                    max_mtime_epoch: e.max_mtime_epoch,
                };
            }
        }
    }
    let st = dir_stat(path);
    if let Ok(mut cache) = size_cache().lock() {
        cache.entries.insert(
            key,
            SizeEntry {
                bytes: st.bytes,
                files: st.files,
                max_mtime_epoch: st.max_mtime_epoch,
                dir_mtime_epoch: fp.0,
                children: fp.1,
            },
        );
        SIZE_CACHE_DIRTY.store(true, Ordering::Relaxed);
    }
    st
}

/// 扫描结束时落盘（仅在确实有更新时写）。
pub fn save_size_cache() {
    if !SIZE_CACHE_DIRTY.swap(false, Ordering::Relaxed) {
        return;
    }
    if let (Some(path), Ok(cache)) = (size_cache_path(), size_cache().lock()) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(s) = serde_json::to_string(&*cache) {
            let _ = std::fs::write(path, s);
        }
    }
}
