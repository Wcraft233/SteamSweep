//! orphan：安装残留 diff 引擎。
//! 覆盖 common / downloading / workshop\content / shadercache 四类位置，
//! 大小计算全部 rayon 并行；common 残留附带 steam_autocloud.vdf 真伪判别。

use crate::manifest::InstalledApp;
use crate::util;
use rayon::prelude::*;
use serde::Serialize;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

/// common 下的内置白名单（非游戏目录，永不判定为残留）。全部小写比较。
const COMMON_WHITELIST: &[&str] = &[
    "steamworks shared",
    "steam controller configs",
    "steamlinuxruntime",
    "steamlinuxruntime_soldier",
    "steamlinuxruntime_sniper",
    "steam client service",
    "steamvr",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Category {
    Common,
    Downloading,
    Workshop,
    Shadercache,
}

impl Category {
    #[allow(dead_code)]
    pub fn label(&self) -> &'static str {
        match self {
            Category::Common => "common（安装目录残留）",
            Category::Downloading => "downloading（未完成的下载）",
            Category::Workshop => "workshop（创意工坊残留）",
            Category::Shadercache => "shadercache（着色器缓存）",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct OrphanDir {
    pub category: Category,
    pub path: PathBuf,
    pub name: String,
    pub library: PathBuf,
    pub size_bytes: u64,
    pub file_count: u64,
    pub last_modified_epoch: u64,
    /// 文件夹内是否发现 steam_autocloud.vdf（黄金标记 => 100% 真 Steam 游戏残留）
    pub has_autocloud: bool,
    pub autocloud_files: Vec<PathBuf>,
    /// confirmed = 含黄金标记，确定是 Steam 游戏；suspected = 需人工确认（可能是手动放入/学习版）
    pub verdict: &'static str,
    pub appid_hint: Option<u32>,
}

#[derive(Debug, Default, Serialize)]
pub struct OrphanReport {
    pub items: Vec<OrphanDir>,
    pub common_count: usize,
    pub common_bytes: u64,
    pub common_autocloud_verified: usize,
    pub other_count: usize,
    pub other_bytes: u64,
}

struct Candidate {
    category: Category,
    path: PathBuf,
    name: String,
    library: PathBuf,
    appid_hint: Option<u32>,
}

pub fn scan(
    libraries: &[PathBuf],
    installed: &BTreeMap<u32, InstalledApp>,
    on_item: Option<&(dyn Fn(&OrphanDir) + Sync)>,
) -> (OrphanReport, Vec<String>) {
    let mut warnings = Vec::new();
    let mut candidates: Vec<Candidate> = Vec::new();

    for lib in libraries {
        let steamapps = lib.join("steamapps");

        // ---- common：文件夹名 ∉ 本库所有 acf installdir（大小写不敏感）----
        let mut installdirs: HashSet<String> = HashSet::new();
        for app in installed.values() {
            if util::normalize_path(&app.library) == util::normalize_path(lib) {
                installdirs.insert(app.installdir.to_lowercase());
            }
        }
        let common = steamapps.join("common");
        if let Ok(rd) = std::fs::read_dir(&common) {
            for entry in rd.flatten() {
                if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().to_string();
                let lower = name.to_lowercase();
                if COMMON_WHITELIST.contains(&lower.as_str()) {
                    continue;
                }
                if !installdirs.contains(&lower) {
                    candidates.push(Candidate {
                        category: Category::Common,
                        path: entry.path(),
                        name,
                        library: lib.clone(),
                        appid_hint: None,
                    });
                }
            }
        } else if common.exists() {
            warnings.push(format!("无法读取 {:?}（已跳过）", common));
        }

        // ---- downloading / workshop\content / shadercache：数字目录 = appid ----
        collect_appid_dirs(
            &steamapps.join("downloading"),
            lib,
            installed,
            Category::Downloading,
            &mut candidates,
        );
        collect_appid_dirs(
            &steamapps.join("workshop").join("content"),
            lib,
            installed,
            Category::Workshop,
            &mut candidates,
        );
        collect_appid_dirs(
            &steamapps.join("shadercache"),
            lib,
            installed,
            Category::Shadercache,
            &mut candidates,
        );
    }

    // ---- 并行计算大小 + 黄金标记探测 ----
    let mut items: Vec<OrphanDir> = candidates
        .par_iter()
        .map(|c| {
            let stat = util::dir_stat_cached(&c.path);
            let autocloud_files = if c.category == Category::Common {
                util::find_files_named(&c.path, "steam_autocloud.vdf", 10, 32)
            } else {
                Vec::new()
            };
            let has_autocloud = !autocloud_files.is_empty();
            let item = OrphanDir {
                category: c.category,
                path: c.path.clone(),
                name: c.name.clone(),
                library: c.library.clone(),
                size_bytes: stat.bytes,
                file_count: stat.files,
                last_modified_epoch: stat.max_mtime_epoch,
                has_autocloud,
                autocloud_files,
                verdict: if c.category == Category::Common {
                    if has_autocloud { "confirmed" } else { "suspected" }
                } else {
                    "confirmed"
                },
                appid_hint: c.appid_hint,
            };
            if let Some(cb) = on_item {
                cb(&item);
            }
            item
        })
        .collect();

    items.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes));

    let mut report = OrphanReport::default();
    for it in &items {
        match it.category {
            Category::Common => {
                report.common_count += 1;
                report.common_bytes += it.size_bytes;
                if it.has_autocloud {
                    report.common_autocloud_verified += 1;
                }
            }
            _ => {
                report.other_count += 1;
                report.other_bytes += it.size_bytes;
            }
        }
    }
    report.items = items;
    (report, warnings)
}

fn collect_appid_dirs(
    dir: &Path,
    lib: &PathBuf,
    installed: &BTreeMap<u32, InstalledApp>,
    category: Category,
    out: &mut Vec<Candidate>,
) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if let Ok(appid) = name.parse::<u32>() {
                if !installed.contains_key(&appid) {
                    out.push(Candidate {
                        category,
                        path: entry.path(),
                        name,
                        library: lib.clone(),
                        appid_hint: Some(appid),
                    });
                }
            }
        }
    }
}
