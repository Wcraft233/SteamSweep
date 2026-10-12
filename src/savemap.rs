//! savemap：云存档索引解析 + Root 映射 + syncstate 判定。
//! 数据源：userdata\<uid>\<appid>\remotecache.vdf + remote\ + ac\ 骨架。
//! 只读：只解析索引并检查本地文件是否存在，绝不修改。

use crate::locator::Account;
use crate::manifest::InstalledApp;
use crate::util;
use crate::vdf;
use rayon::prelude::*;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

/// 最近进度未同步的阈值：localtime 比 remotetime 新超过 1 小时。
const UNSYNCED_GRACE_SECS: u64 = 3600;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum SyncVerdict {
    /// 全部 syncstate=2 且本地存在 → 🔴 删除即永久丢失
    NeverUploadedLocalPresent,
    /// 全部 syncstate=2 但本地已不存在 → ⚪ 仅显示
    NeverUploadedLocalGone,
    /// syncstate=1 但 localtime 显著新于 remotetime → 🟡 最近进度或未上传
    PossiblyUnsynced,
    /// syncstate 混合（既有 1 又有 2）→ 有 2 在本地则视为红色
    Mixed,
    /// 全部已同步
    Synced,
    /// 无 remotecache.vdf 索引
    NoIndex,
}

impl SyncVerdict {
    pub fn label(&self) -> &'static str {
        match self {
            SyncVerdict::NeverUploadedLocalPresent => "🔴 疑似从未上传云端（删除即永久丢失）",
            SyncVerdict::NeverUploadedLocalGone => "⚪ 云端有副本、本地已清",
            SyncVerdict::PossiblyUnsynced => "🟡 最近进度或未上传",
            SyncVerdict::Mixed => "🟡 同步状态混合",
            SyncVerdict::Synced => "已同步",
            SyncVerdict::NoIndex => "无云索引",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SaveFile {
    pub rel_path: String,
    pub root: u32,
    pub size: u64,
    pub localtime: u64,
    pub remotetime: u64,
    pub syncstate: i64,
    /// Root 映射展开后的本地绝对路径（root 未知/不可解析时为 None）
    pub resolved_path: Option<PathBuf>,
    pub exists_locally: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct AppSaveRecord {
    pub appid: u32,
    pub account_id: u32,
    pub name: Option<String>,
    pub installed: bool,
    /// userdata\<uid>\<appid> 整个目录的本地占用
    pub userdata_bytes: u64,
    pub userdata_files: u64,
    /// remote\ 子目录实际占用与文件数
    pub remote_bytes: u64,
    pub remote_files: u64,
    pub has_index: bool,
    pub is_empty_shell: bool,
    /// remote\ 有文件但 remotecache.vdf 丢失
    pub remote_without_index: bool,
    pub cloud_files: usize,
    pub cloud_bytes: u64,
    pub sync: SyncVerdict,
    /// 已解析且本地真实存在的存档所在目录（去重）
    pub local_save_dirs: Vec<PathBuf>,
    /// ac\ 骨架镜像出的目录结构（相对 ac\）
    pub ac_skeleton: Vec<String>,
    pub files: Vec<SaveFile>,
}

#[derive(Debug, Default, Serialize)]
pub struct SaveReport {
    pub records: Vec<AppSaveRecord>,
    pub total_apps: usize,
    pub orphan_apps: usize,
    pub empty_shells: usize,
    pub remote_without_index: usize,
    pub red_alert_apps: usize,
    pub userdata_total_bytes: u64,
    pub userdata_orphan_bytes: u64,
}

/// Root 编号映射表（方案 §3.2，已本机实测验证）。未知 root 返回 None → 报告标记"未知位置"。
pub(crate) fn root_base(root: u32, steam_path: &Path, uid: u32, appid: u32, installed: Option<&InstalledApp>) -> Option<PathBuf> {
    match root {
        // Default：Steam\userdata\<uid>\<appid>\remote\
        0 => Some(
            steam_path
                .join("userdata")
                .join(uid.to_string())
                .join(appid.to_string())
                .join("remote"),
        ),
        // 游戏安装目录（仅游戏仍安装时可解析）
        1 => installed.map(|a| {
            a.library
                .join("steamapps")
                .join("common")
                .join(&a.installdir)
        }),
        // WinMyDocuments
        2 => util::user_profile().map(|p| p.join("Documents")),
        // WinAppDataLocal
        3 => util::env_path("LOCALAPPDATA"),
        // WinAppDataRoaming
        6 => util::env_path("APPDATA"),
        // WinSavedGames
        9 => util::user_profile().map(|p| p.join("Saved Games")),
        // SteamCloudDocuments
        11 => util::user_profile().map(|p| p.join("Documents").join("Steam Cloud")),
        // WinAppDataLocalLow
        12 => util::user_profile().map(|p| p.join("AppData").join("LocalLow")),
        _ => None,
    }
}

struct ScanTask {
    account_id: u32,
    appid: u32,
    dir: PathBuf,
}

/// appinfo ufs 的命名 root（"WinSavedGames" 等）→ 编号 root → 本地基路径。
/// Mac/Linux 专用 root 在 Windows 上不可解析，返回 None。
pub(crate) fn named_root_base(
    name: &str,
    steam_path: &Path,
    uid: u32,
    appid: u32,
    installed: Option<&InstalledApp>,
) -> Option<PathBuf> {
    let n = name.trim().trim_start_matches('%').trim_end_matches('%');
    let root = match n.to_ascii_lowercase().as_str() {
        "winmydocuments" | "documents" => 2,
        "winappdatalocal" | "appdatalocal" => 3,
        "winappdataroaming" | "appdataroaming" => 6,
        "winsavedgames" | "savedgames" => 9,
        "steamclouddocuments" => 11,
        "winappdatalocallow" | "appdatalocallow" => 12,
        "gameinstall" | "game_install" | "install" => 1,
        _ => return None,
    };
    root_base(root, steam_path, uid, appid, installed)
}

/// M3：appinfo ufs 补定位。为"已卸载且 remotecache 索引缺失"的游戏，
/// 用 Steam 官方 Auto-Cloud 配置补定位仍存在的存档目录，合成 NoIndex 记录。
pub fn ufs_supplement(
    steam_path: &Path,
    accounts: &[Account],
    installed: &BTreeMap<u32, InstalledApp>,
    indexed: &HashSet<u32>,
) -> Vec<AppSaveRecord> {
    let blocks = crate::appinfo::parse_ufs(steam_path);
    let primary = accounts.first().map(|a| a.account_id).unwrap_or(0);
    let mut extra: HashMap<u32, Vec<PathBuf>> = HashMap::new();
    for b in &blocks {
        if installed.contains_key(&b.appid) || indexed.contains(&b.appid) {
            continue; // 已安装或已有云索引的不需要补
        }
        if b.path.contains('%') || b.path.contains('{') {
            continue; // 占位符无法解析的跳过
        }
        for acc in accounts {
            if let Some(base) = named_root_base(&b.root, steam_path, acc.account_id, b.appid, None) {
                let rel = b.path.replace('/', "\\");
                let dir = if rel.is_empty() { base.clone() } else { base.join(&rel) };
                if dir.is_dir() {
                    extra.entry(b.appid).or_default().push(dir);
                }
            }
        }
    }
    let mut out = Vec::new();
    for (appid, mut dirs) in extra {
        dirs.sort();
        dirs.dedup();
        let mut bytes = 0u64;
        let mut files = 0u64;
        for d in &dirs {
            let st = util::dir_stat_cached(d);
            bytes += st.bytes;
            files += st.files;
        }
        if files == 0 {
            continue; // 空目录不出记录
        }
        out.push(AppSaveRecord {
            appid,
            account_id: primary,
            name: None,
            installed: false,
            userdata_bytes: bytes,
            userdata_files: files,
            remote_bytes: 0,
            remote_files: 0,
            has_index: false,
            is_empty_shell: false,
            remote_without_index: false,
            cloud_files: 0,
            cloud_bytes: 0,
            sync: SyncVerdict::NoIndex,
            local_save_dirs: dirs,
            ac_skeleton: Vec::new(),
            files: Vec::new(),
        });
    }
    out
}

pub fn scan(
    steam_path: &Path,
    accounts: &[Account],
    installed: &BTreeMap<u32, InstalledApp>,
    on_item: Option<&(dyn Fn(&AppSaveRecord) + Sync)>,
) -> (SaveReport, Vec<String>) {
    let mut warnings = Vec::new();
    let mut tasks: Vec<ScanTask> = Vec::new();

    for acc in accounts {
        let user_dir = steam_path.join("userdata").join(acc.account_id.to_string());
        let rd = match std::fs::read_dir(&user_dir) {
            Ok(r) => r,
            Err(_) => continue, // 该账号本机无 userdata，正常
        };
        for entry in rd.flatten() {
            if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if let Ok(appid) = name.parse::<u32>() {
                tasks.push(ScanTask {
                    account_id: acc.account_id,
                    appid,
                    dir: entry.path(),
                });
            }
        }
    }

    let results: Vec<Result<AppSaveRecord, String>> = tasks
        .par_iter()
        .map(|t| scan_one(steam_path, t, installed))
        .collect();

    let mut report = SaveReport::default();
    for r in results {
        match r {
            Ok(rec) => {
                report.total_apps += 1;
                report.userdata_total_bytes += rec.userdata_bytes;
                if rec.is_empty_shell {
                    report.empty_shells += 1;
                }
                if rec.remote_without_index {
                    report.remote_without_index += 1;
                }
                if rec.sync == SyncVerdict::NeverUploadedLocalPresent {
                    report.red_alert_apps += 1;
                }
                if !rec.installed {
                    report.orphan_apps += 1;
                    report.userdata_orphan_bytes += rec.userdata_bytes;
                }
                if let Some(cb) = on_item {
                    cb(&rec);
                }
                report.records.push(rec);
            }
            Err(e) => warnings.push(e),
        }
    }
    report
        .records
        .sort_by(|a, b| b.userdata_bytes.cmp(&a.userdata_bytes));
    (report, warnings)
}

fn scan_one(
    steam_path: &Path,
    task: &ScanTask,
    installed: &BTreeMap<u32, InstalledApp>,
) -> Result<AppSaveRecord, String> {
    let app = installed.get(&task.appid);
    let dir_stat = util::dir_stat_cached(&task.dir);

    // remote\ 实际内容（索引可能丢失，必须直扫）
    let remote_dir = task.dir.join("remote");
    let remote_stat = if remote_dir.exists() {
        util::dir_stat_cached(&remote_dir)
    } else {
        Default::default()
    };

    // remotecache.vdf 索引
    let index_path = task.dir.join("remotecache.vdf");
    let mut files: Vec<SaveFile> = Vec::new();
    let has_index = index_path.exists();
    if has_index {
        match std::fs::read_to_string(&index_path)
            .map_err(|e| e.to_string())
            .and_then(|t| vdf::parse(&t))
        {
            Ok(root) => {
                for (rel, node) in vdf::extract_file_entries(&root) {
                    let root_id = node.get_u64("root").unwrap_or(0) as u32;
                    let base = root_base(root_id, steam_path, task.account_id, task.appid, app);
                    let resolved = base.map(|b| b.join(rel.replace('/', "\\")));
                    let exists = resolved.as_ref().map(|p| p.exists()).unwrap_or(false);
                    files.push(SaveFile {
                        rel_path: rel,
                        root: root_id,
                        size: node.get_u64("size").unwrap_or(0),
                        localtime: node.get_u64("localtime").unwrap_or(0),
                        remotetime: node.get_u64("remotetime").unwrap_or(0),
                        syncstate: node.get_str("syncstate").and_then(|s| s.parse().ok()).unwrap_or(-1),
                        resolved_path: resolved,
                        exists_locally: exists,
                    });
                }
            }
            Err(e) => {
                // 防御式：索引损坏不致命，按无索引处理
                return Ok(build_record(
                    task, app, dir_stat, remote_stat, false, Vec::new(),
                    Some(format!("remotecache.vdf 解析失败：{}", e)),
                ));
            }
        }
    }

    Ok(build_record(task, app, dir_stat, remote_stat, has_index, files, None))
}

fn build_record(
    task: &ScanTask,
    app: Option<&InstalledApp>,
    dir_stat: util::DirStat,
    remote_stat: util::DirStat,
    has_index: bool,
    files: Vec<SaveFile>,
    parse_note: Option<String>,
) -> AppSaveRecord {
    let cloud_bytes: u64 = files.iter().map(|f| f.size).sum();

    // ---- syncstate 判定（方案 §模块 E）----
    let sync = if files.is_empty() {
        SyncVerdict::NoIndex
    } else {
        let all_two = files.iter().all(|f| f.syncstate == 2);
        let any_two = files.iter().any(|f| f.syncstate == 2);
        if all_two {
            if files.iter().any(|f| f.exists_locally) {
                SyncVerdict::NeverUploadedLocalPresent
            } else {
                SyncVerdict::NeverUploadedLocalGone
            }
        } else if any_two {
            SyncVerdict::Mixed
        } else {
            let stale = files.iter().any(|f| {
                f.syncstate == 1
                    && f.localtime > 0
                    && f.localtime > f.remotetime.saturating_add(UNSYNCED_GRACE_SECS)
            });
            if stale {
                SyncVerdict::PossiblyUnsynced
            } else {
                SyncVerdict::Synced
            }
        }
    };

    // ---- 本地实际存在的存档目录（去重）----
    let mut seen: HashSet<String> = HashSet::new();
    let mut local_save_dirs: Vec<PathBuf> = Vec::new();
    for f in &files {
        if f.exists_locally {
            if let Some(rp) = &f.resolved_path {
                if let Some(parent) = rp.parent() {
                    let key = util::normalize_path(parent);
                    if seen.insert(key) {
                        local_save_dirs.push(parent.to_path_buf());
                    }
                }
            }
        }
    }

    // ---- ac\ 骨架（镜像云存档的根映射目录结构）----
    let ac_dir = task.dir.join("ac");
    let mut ac_skeleton: Vec<String> = Vec::new();
    if ac_dir.exists() {
        for entry in walkdir::WalkDir::new(&ac_dir)
            .follow_links(false)
            .into_iter()
            .flatten()
        {
            if entry.file_type().is_dir() && entry.path() != ac_dir {
                if let Ok(rel) = entry.path().strip_prefix(&ac_dir) {
                    ac_skeleton.push(rel.to_string_lossy().to_string());
                    if ac_skeleton.len() >= 64 {
                        break;
                    }
                }
            }
        }
    }

    let _ = parse_note; // 解析失败备注目前并入无索引路径，M2 再展示细节

    AppSaveRecord {
        appid: task.appid,
        account_id: task.account_id,
        name: app.map(|a| a.name.clone()),
        installed: app.is_some(),
        userdata_bytes: dir_stat.bytes,
        userdata_files: dir_stat.files,
        remote_bytes: remote_stat.bytes,
        remote_files: remote_stat.files,
        has_index,
        is_empty_shell: dir_stat.files == 0,
        remote_without_index: remote_stat.files > 0 && !has_index,
        cloud_files: files.len(),
        cloud_bytes,
        sync,
        local_save_dirs,
        ac_skeleton,
        files,
    }
}
