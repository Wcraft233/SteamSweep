//! 启发式扫描（方案 §模块 D）：在 remotecache 覆盖不到的区域发现存档目录。
//!
//! 规则：
//! - 黄金标记：目录内含 steam_autocloud.vdf → ✅ 确认（Steam 官方同步过的存档目录）
//! - 引擎特征：UE `Saved\SaveGames` / Unity LocalLow 两级 / Godot app_userdata / RenPy
//! - 目录名特征：saves / savegames / savedata / profiles / checkpoints / 存档 …
//! - 文件特征：.sav .save .lsv .sl2 .ess .srm；save|slot|profile 开头的 .dat/.bin/.json/.xml
//! - 灰名单：常见非游戏软件（VS Code、Thunderbird、obs-studio、mihomo、BakaXL、Vortex…）整树剪枝
//!
//! 命中目录分三级：✅确认 / ⚠️疑似 / ❓未知；与云索引重叠的目录标记为 covered（不算新发现）。

use crate::util;
use crate::vdf;
use rayon::prelude::*;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;

const SAVE_EXTS: &[&str] = &["sav", "save", "lsv", "sl2", "ess", "srm"];
const GENERIC_EXTS: &[&str] = &["dat", "bin", "json", "xml"];
const SAVE_PREFIXES: &[&str] = &["save", "slot", "profile"];

/// 灰名单（小写目录名整树剪枝）。判定结果来自方案 §模块 D 实测误报清单 + 常见非游戏软件。
const GRAYLIST: &[&str] = &[
    "microsoft", "windows", "google", "chrome", "chromium", "firefox", "mozilla",
    "thunderbird", "code", "vscode", "visual studio code", "obs-studio", "obs",
    "mihomo", "clash", "clash verge", "clashverge", "bakaxl", "vortex", "discord",
    "spotify", "slack", "jetbrains", "cursor", "nvidia", "adobe", "edge", "brave",
    "opera", "git", "github", "zoom", "wechat", "tencent", "netease", "gsm backups",
    "packages", "fonts", "gpuCache", "pip", "npm", "nuget", "docker", "kimi-desktop",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Tier {
    Confirmed,
    Suspected,
    Unknown,
}

#[derive(Debug, Clone, Serialize)]
pub struct HeuristicDir {
    pub path: PathBuf,
    pub tier: Tier,
    pub reasons: Vec<String>,
    /// 黄金标记中的 accountid（可区分账号）
    pub autocloud_account: Option<u32>,
    /// 是否已被云索引（remotecache）覆盖 —— 覆盖则不是"新发现"
    pub covered_by_cloud_index: bool,
    pub size_bytes: u64,
    pub file_count: u64,
}

#[derive(Debug, Default, Serialize)]
pub struct HeuristicReport {
    pub confirmed: Vec<HeuristicDir>,
    pub suspected: Vec<HeuristicDir>,
    pub unknown: Vec<HeuristicDir>,
    pub confirmed_new: usize,
    pub suspected_new: usize,
    pub unknown_new: usize,
    pub new_total_bytes: u64,
    pub graylisted_pruned: usize,
    pub stats_bin_appids: usize,
    pub stats_bin_orphan_appids: usize,
}

#[derive(Clone, Copy, PartialEq)]
enum RootKind {
    Documents,
    SavedGames,
    Local,
    LocalLow,
    Roaming,
}

struct RootSpec {
    path: PathBuf,
    kind: RootKind,
    max_depth: u32,
}

enum Ev {
    Dir { root_idx: usize, path: PathBuf },
    Golden { parent: PathBuf, file: PathBuf },
    SaveExt { parent: PathBuf },
    GenericSave { parent: PathBuf },
}

/// 扫描模式：Fast 只找黄金标记（快）；Full 附加引擎/目录名/扩展名规则（慢，M3 再用）。
#[derive(Clone, Copy, PartialEq)]
pub enum ScanMode {
    Fast,
    Full,
}

#[derive(Default)]
struct DirInfo {
    golden_account: Option<u32>,
    /// 黄金标记文件路径（accountid 在遍历结束后统一带超时读取，防止占位文件卡死遍历）
    golden_file: Option<PathBuf>,
    ue: bool,
    unity: bool,
    godot: bool,
    renpy: bool,
    name_rule: bool,
    save_ext_files: u32,
    generic_save_files: u32,
}

impl DirInfo {
    fn flagged(&self) -> bool {
        self.golden_file.is_some()
            || self.ue
            || self.unity
            || self.godot
            || self.renpy
            || self.name_rule
            || self.save_ext_files > 0
            || self.generic_save_files > 0
    }
}

/// 快速版：只扫常见目录（AppData×3 / Documents / Saved Games）的黄金标记 + stats bin。
pub fn scan_fast(
    steam_path: &Path,
    installed_appids: &HashSet<u32>,
    cloud_covered_dirs: &HashSet<String>,
    on_item: Option<&(dyn Fn(&HeuristicDir) + Sync)>,
) -> (HeuristicReport, Vec<String>) {
    scan_inner(ScanMode::Fast, steam_path, installed_appids, cloud_covered_dirs, on_item)
}

/// 完整版（M3 备用）：附加引擎特征 / 目录名 / 扩展名规则。
#[allow(dead_code)]
pub fn scan_full(
    steam_path: &Path,
    installed_appids: &HashSet<u32>,
    cloud_covered_dirs: &HashSet<String>,
) -> (HeuristicReport, Vec<String>) {
    scan_inner(ScanMode::Full, steam_path, installed_appids, cloud_covered_dirs, None)
}

fn scan_inner(
    mode: ScanMode,
    steam_path: &Path,
    installed_appids: &HashSet<u32>,
    cloud_covered_dirs: &HashSet<String>,
    on_item: Option<&(dyn Fn(&HeuristicDir) + Sync)>,
) -> (HeuristicReport, Vec<String>) {
    let warnings = Vec::new();
    let mut report = HeuristicReport::default();

    // ---- stats bin 痕迹（appcache\stats\UserGameStatsSchema_<appid>.bin）----
    let stats_dir = steam_path.join("appcache").join("stats");
    if let Ok(rd) = std::fs::read_dir(&stats_dir) {
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if let Some(appid) = parse_stats_bin_name(&name) {
                report.stats_bin_appids += 1;
                if !installed_appids.contains(&appid) {
                    report.stats_bin_orphan_appids += 1;
                }
            }
        }
    }

    // ---- 扫描根 ----
    let mut roots: Vec<RootSpec> = Vec::new();
    if let Some(up) = util::user_profile() {
        roots.push(RootSpec { path: up.join("Documents"), kind: RootKind::Documents, max_depth: 6 });
        roots.push(RootSpec { path: up.join("Saved Games"), kind: RootKind::SavedGames, max_depth: 5 });
        roots.push(RootSpec { path: up.join("AppData").join("LocalLow"), kind: RootKind::LocalLow, max_depth: 5 });
    }
    if let Some(local) = util::env_path("LOCALAPPDATA") {
        roots.push(RootSpec { path: local, kind: RootKind::Local, max_depth: 5 });
    }
    if let Some(roaming) = util::env_path("APPDATA") {
        roots.push(RootSpec { path: roaming, kind: RootKind::Roaming, max_depth: 5 });
    }
    roots.retain(|r| r.path.exists());

    let graylist: HashSet<String> = GRAYLIST.iter().map(|s| s.to_lowercase()).collect();
    let gray_count = AtomicUsize::new(0);
    let dir_counter = AtomicUsize::new(0);
    let walk_done = std::sync::atomic::AtomicBool::new(false);
    let t_walk0 = std::time::Instant::now();

    // ---- 并行遍历（jwalk，根内多线程），事件经由有界 channel 汇总（背压防止内存膨胀）----
    // 文件名规则匹配在 walker 线程内完成，只把命中项发回，事件量降 99%+。
    let (tx, rx) = mpsc::sync_channel::<Ev>(8192);
    let mut infos: HashMap<PathBuf, DirInfo> = HashMap::new();
    let t_walk = std::time::Instant::now();
    std::thread::scope(|s| {
        // 引用先取出（&T 是 Copy，可同时给两个闭包用）；tx 必须 move 进 walker 闭包，
        // 否则外层栈帧持有的原始 Sender 永不 drop，接收端永远等不到 channel 关闭（死锁）。
        let roots_r = &roots;
        let graylist_r = &graylist;
        let gray_count_r = &gray_count;
        let dir_counter_r = &dir_counter;
        let walk_done_r = &walk_done;
        s.spawn(move || {
            // 根之间顺序（jwalk 根内已并行，且避免多个 jwalk 互抢 rayon 池触发 busy_timeout）
            for (idx, r) in roots_r.iter().enumerate() {
                walk(&r.path, r.max_depth, &tx, idx, graylist_r, gray_count_r, mode, dir_counter_r);
            }
            eprintln!(
                "  [heuristic] walker 完成（{} 个目录）",
                dir_counter_r.load(Ordering::Relaxed)
            );
            drop(tx); // 显式关闭发送端
        });
        s.spawn(move || {
            while !walk_done_r.load(Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_secs(5));
                if walk_done_r.load(Ordering::Relaxed) {
                    break;
                }
                eprintln!(
                    "  [heuristic] 遍历中…已扫目录 {}（{:.0?}）",
                    dir_counter_r.load(Ordering::Relaxed),
                    t_walk0.elapsed()
                );
            }
        });

        while let Ok(ev) = rx.recv() {
            match ev {
                Ev::Dir { root_idx, path } => {
                    let info = infos.entry(path.clone()).or_default();
                    classify_dir(&roots[root_idx], &path, info);
                }
                Ev::Golden { parent, file } => {
                    // 只记录路径，不读文件内容（OneDrive 占位文件读内容可能触发网络回拉卡死）
                    let info = infos.entry(parent).or_default();
                    if info.golden_file.is_none() {
                        info.golden_file = Some(file);
                    }
                }
                Ev::SaveExt { parent } => {
                    infos.entry(parent).or_default().save_ext_files += 1;
                }
                Ev::GenericSave { parent } => {
                    infos.entry(parent).or_default().generic_save_files += 1;
                }
            }
        }
        walk_done.store(true, Ordering::Relaxed);
        eprintln!("  [heuristic] 接收循环已退出");
    });
    eprintln!(
        "  [heuristic] 目录遍历完成：{} 个命中目录，耗时 {:.1?}",
        infos.len(),
        t_walk.elapsed()
    );
    report.graylisted_pruned = gray_count.load(Ordering::Relaxed);

    // ---- 遍历结束后再统一读取黄金标记的 accountid（带 2s 超时，防止占位文件卡死）----
    {
        let golden_paths: Vec<PathBuf> = infos
            .iter()
            .filter_map(|(p, i)| i.golden_file.as_ref().map(|_| p.clone()))
            .collect();
        let accounts: HashMap<PathBuf, Option<u32>> = golden_paths
            .par_iter()
            .map(|p| {
                let acc = infos
                    .get(p)
                    .and_then(|i| i.golden_file.as_ref())
                    .and_then(|f| read_autocloud_account(f));
                (p.clone(), acc)
            })
            .collect();
        for (p, acc) in accounts {
            if let Some(info) = infos.get_mut(&p) {
                info.golden_account = acc;
            }
        }
    }

    // ---- 汇总候选 + 合并"祖先已命中"的子目录 ----
    let mut candidates: Vec<(PathBuf, DirInfo)> = infos
        .into_iter()
        .filter(|(_, i)| i.flagged())
        .map(|(p, i)| (p, i))
        .collect();
    candidates.sort_by(|a, b| a.0.cmp(&b.0));

    let cand_set: HashSet<String> = candidates
        .iter()
        .map(|(p, _)| util::normalize_path(p))
        .collect();
    let merged: Vec<(PathBuf, DirInfo)> = candidates
        .into_iter()
        .filter(|(p, _)| !has_flagged_ancestor(p, &cand_set))
        .collect();

    // ---- 分级 + 覆盖判定 ----
    let mut dirs: Vec<HeuristicDir> = merged
        .into_par_iter()
        .map(|(path, info)| {
            let covered = is_covered(&path, cloud_covered_dirs);
            let mut reasons: Vec<String> = Vec::new();
            if info.golden_file.is_some() {
                reasons.push("黄金标记 steam_autocloud.vdf".to_string());
            }
            if info.ue {
                reasons.push("UE4/UE5 引擎特征 Saved\\SaveGames".to_string());
            }
            if info.unity {
                reasons.push("Unity 引擎特征 LocalLow\\<Company>\\<Game>".to_string());
            }
            if info.godot {
                reasons.push("Godot 引擎特征 app_userdata".to_string());
            }
            if info.renpy {
                reasons.push("RenPy 引擎特征".to_string());
            }
            if info.name_rule {
                reasons.push("目录名特征（saves/savegames/profiles/存档…）".to_string());
            }
            if info.save_ext_files > 0 {
                reasons.push(format!("含 {} 个存档扩展名文件（.sav/.save/.lsv/…）", info.save_ext_files));
            }
            if info.generic_save_files > 0 {
                reasons.push(format!("含 {} 个 save/slot/profile 前缀文件", info.generic_save_files));
            }
            if covered {
                reasons.push("已被云索引覆盖".to_string());
            }

            let tier = if info.golden_file.is_some() || covered {
                Tier::Confirmed
            } else if info.ue
                || info.godot
                || info.renpy
                || (info.unity && (info.save_ext_files > 0 || info.name_rule || info.generic_save_files > 0))
                || (info.name_rule && info.save_ext_files > 0)
            {
                Tier::Suspected
            } else if info.unity || info.name_rule || info.save_ext_files > 0 {
                Tier::Unknown
            } else {
                Tier::Unknown
            };

            let stat = util::dir_stat_cached(&path);
            let dir = HeuristicDir {
                path,
                tier,
                reasons,
                autocloud_account: info.golden_account,
                covered_by_cloud_index: covered,
                size_bytes: stat.bytes,
                file_count: stat.files,
            };
            if let Some(cb) = on_item {
                cb(&dir);
            }
            dir
        })
        .collect();

    dirs.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes));
    eprintln!(
        "  [heuristic] 分级与体积统计完成，累计耗时 {:.1?}",
        t_walk.elapsed()
    );

    for d in dirs {
        let is_new = !d.covered_by_cloud_index;
        match d.tier {
            Tier::Confirmed => {
                if is_new {
                    report.confirmed_new += 1;
                    report.new_total_bytes += d.size_bytes;
                }
                report.confirmed.push(d);
            }
            Tier::Suspected => {
                if is_new {
                    report.suspected_new += 1;
                    report.new_total_bytes += d.size_bytes;
                }
                report.suspected.push(d);
            }
            Tier::Unknown => {
                if is_new {
                    report.unknown_new += 1;
                    report.new_total_bytes += d.size_bytes;
                }
                report.unknown.push(d);
            }
        }
    }

    (report, warnings)
}

/// jwalk 并行遍历一个根：目录只发事件（Full 模式），文件就地做规则匹配、只发命中项。
/// 灰名单在 process_read_dir 回调里整树剪枝（不再 yield，也不会深入）。
fn walk(
    dir: &Path,
    max_depth: u32,
    tx: &mpsc::SyncSender<Ev>,
    root_idx: usize,
    graylist: &HashSet<String>,
    gray_count: &AtomicUsize,
    mode: ScanMode,
    dir_counter: &AtomicUsize,
) {
    let graylist_owned = graylist.clone();
    let gray_local = std::sync::Arc::new(AtomicUsize::new(0));
    let gray_local_c = gray_local.clone();
    let walker = jwalk::WalkDir::new(dir)
        // 旧实现名义限深、实际无限递归；这里取名义深度 +4 兜底深层存档目录
        .max_depth(max_depth as usize + 4)
        .follow_links(false)
        .skip_hidden(false)
        .process_read_dir(move |_depth, _path, _state, children| {
            children.retain(|res| {
                if let Ok(e) = res {
                    if e.file_type.is_dir() {
                        let name_lower = e.file_name.to_string_lossy().to_lowercase();
                        if graylist_owned.contains(&name_lower) {
                            gray_local_c.fetch_add(1, Ordering::Relaxed);
                            return false; // 整树剪枝
                        }
                    }
                }
                true
            });
        });

    for entry in walker.into_iter().flatten() {
        let ft = entry.file_type();
        if ft.is_symlink() {
            continue; // 跳过符号链接/目录联接
        }
        if ft.is_dir() {
            dir_counter.fetch_add(1, Ordering::Relaxed);
            if mode == ScanMode::Full {
                let _ = tx.send(Ev::Dir { root_idx, path: entry.path() });
            }
        } else if ft.is_file() {
            // 规则匹配就地完成，只把命中项发回（否则数百万文件事件会压垮 channel）
            let lower = entry.file_name().to_string_lossy().to_lowercase();
            if lower == "steam_autocloud.vdf" {
                let file = entry.path();
                if let Some(parent) = file.parent() {
                    let _ = tx.send(Ev::Golden {
                        parent: parent.to_path_buf(),
                        file,
                    });
                }
            } else if mode == ScanMode::Full {
                let file = entry.path();
                if let Some(parent) = file.parent() {
                    if is_save_ext_file(&lower) {
                        let _ = tx.send(Ev::SaveExt {
                            parent: parent.to_path_buf(),
                        });
                    } else if is_generic_save_file(&lower) {
                        let _ = tx.send(Ev::GenericSave {
                            parent: parent.to_path_buf(),
                        });
                    }
                }
            }
        }
    }
    gray_count.fetch_add(gray_local.load(Ordering::Relaxed), Ordering::Relaxed);
}

fn classify_dir(root: &RootSpec, path: &Path, info: &mut DirInfo) {
    let rel: Vec<String> = match path.strip_prefix(&root.path) {
        Ok(r) => r
            .components()
            .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
            .collect(),
        Err(_) => return,
    };
    if rel.is_empty() {
        return;
    }
    if let Some(name) = rel.last() {
        info.name_rule = name_rule(name);
    }
    match root.kind {
        RootKind::Local => {
            // UE4/UE5: ...\<X>\Saved\SaveGames
            if rel.len() >= 2 {
                for w in rel.windows(2) {
                    if w[0] == "saved" && w[1] == "savegames" {
                        info.ue = true;
                        break;
                    }
                }
            }
        }
        RootKind::LocalLow => {
            // Unity: LocalLow\<Company>\<Game>
            if rel.len() == 2 {
                info.unity = true;
            }
        }
        RootKind::Roaming => {
            if rel.len() == 3 && rel[0] == "godot" && rel[1] == "app_userdata" {
                info.godot = true;
            }
            if rel.len() == 2 && rel[0] == "renpy" {
                info.renpy = true;
            }
        }
        _ => {}
    }
}

fn name_rule(name_lower: &str) -> bool {
    matches!(
        name_lower,
        "save" | "saves" | "savegame" | "savegames" | "savedata" | "save_data"
            | "save-files" | "savefiles" | "profile" | "profiles" | "checkpoint"
            | "checkpoints" | "saved games" | "存档" | "存檔"
    ) || name_lower.contains("存档")
        || name_lower.contains("存檔")
}

fn is_save_ext_file(name_lower: &str) -> bool {
    match name_lower.rsplit_once('.') {
        Some((_, ext)) => SAVE_EXTS.contains(&ext),
        None => false,
    }
}

fn is_generic_save_file(name_lower: &str) -> bool {
    if let Some((stem, ext)) = name_lower.rsplit_once('.') {
        if GENERIC_EXTS.contains(&ext) {
            for p in SAVE_PREFIXES {
                if stem == *p {
                    return true;
                }
                if let Some(rest) = stem.strip_prefix(p) {
                    if rest
                        .chars()
                        .next()
                        .map(|c| c.is_ascii_digit() || c == '_' || c == '-')
                        .unwrap_or(false)
                    {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// 读取黄金标记中的 accountid。带 2 秒超时：OneDrive 等占位文件读内容会触发网络回拉，
/// 网络不通时可能永久阻塞，超时直接放弃（文件存在本身就是信号，accountid 属于锦上添花）。
fn read_autocloud_account(path: &Path) -> Option<u32> {
    let p = path.to_path_buf();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = (|| {
            let text = std::fs::read_to_string(&p).ok()?;
            let root = vdf::parse(&text).ok()?;
            root.get_u64("accountid").map(|v| v as u32)
        })();
        let _ = tx.send(result);
    });
    rx.recv_timeout(std::time::Duration::from_secs(2)).ok().flatten()
}

fn parse_stats_bin_name(name: &str) -> Option<u32> {
    let lower = name.to_lowercase();
    let rest = lower.strip_prefix("usergamestatsschema_")?;
    let num = rest.strip_suffix(".bin")?;
    num.parse::<u32>().ok()
}

fn has_flagged_ancestor(path: &Path, cand_set: &HashSet<String>) -> bool {
    let mut cur = path.parent();
    while let Some(p) = cur {
        if cand_set.contains(&util::normalize_path(p)) {
            return true;
        }
        cur = p.parent();
    }
    false
}

fn is_covered(path: &Path, covered: &HashSet<String>) -> bool {
    let mut cur = Some(path);
    while let Some(p) = cur {
        if covered.contains(&util::normalize_path(p)) {
            return true;
        }
        cur = p.parent();
    }
    false
}
