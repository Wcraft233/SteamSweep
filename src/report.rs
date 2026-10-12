//! report：汇总各模块结果，输出终端摘要 + JSON 报告文件。

use crate::heuristic::{HeuristicReport, Tier};
use crate::locator::Location;
use crate::manifest::InstalledApp;
use crate::orphan::{Category, OrphanReport};
use crate::savemap::{SaveReport, SyncVerdict};
use crate::util;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Serialize)]
pub struct EnvInfo {
    pub steam_path: PathBuf,
    pub libraries: Vec<PathBuf>,
    pub accounts: Vec<crate::locator::Account>,
    pub installed_games: usize,
}

#[derive(Serialize)]
pub struct Report {
    pub tool: &'static str,
    pub generated_at: String,
    pub environment: EnvInfo,
    pub install_leftovers: OrphanReport,
    pub save_leftovers: SaveReport,
    pub heuristic: HeuristicReport,
    pub warnings: Vec<String>,
}

pub fn build(
    loc: &Location,
    installed: &BTreeMap<u32, InstalledApp>,
    orphans: OrphanReport,
    saves: SaveReport,
    heuristic: HeuristicReport,
    warnings: Vec<String>,
) -> Report {
    Report {
        tool: "SteamSweep M1 (read-only scanner)",
        generated_at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S %Z").to_string(),
        environment: EnvInfo {
            steam_path: loc.steam_path.clone(),
            libraries: loc.libraries.clone(),
            accounts: loc.accounts.clone(),
            installed_games: installed.len(),
        },
        install_leftovers: orphans,
        save_leftovers: saves,
        heuristic,
        warnings,
    }
}

pub fn write_json(report: &Report, path: &std::path::Path) -> Result<(), String> {
    let json = serde_json::to_string_pretty(report).map_err(|e| e.to_string())?;
    std::fs::write(path, json).map_err(|e| format!("写入 {:?} 失败：{}", path, e))
}

pub fn print_summary(report: &Report) {
    let line = "=".repeat(68);
    println!("\n{}", line);
    println!("SteamSweep 扫描报告 · {}", report.generated_at);
    println!("{}", line);

    // ---- 环境 ----
    let env = &report.environment;
    println!("\n【环境】");
    println!("  Steam 路径 : {}", env.steam_path.display());
    for lib in &env.libraries {
        println!("  库文件夹   : {}", lib.display());
    }
    for acc in &env.accounts {
        println!(
            "  账号       : {} (account_id={}, SteamID64={})",
            acc.persona.as_deref().unwrap_or("<未知昵称>"),
            acc.account_id,
            acc.steam_id64
        );
    }
    println!("  已安装游戏 : {} 个", env.installed_games);

    // ---- 安装残留 ----
    let il = &report.install_leftovers;
    println!("\n【安装残留】");
    println!(
        "  common 无主文件夹 : {} 个 / {}（其中含黄金标记、确认真 Steam 残留 {} 个）",
        il.common_count,
        util::human_size(il.common_bytes),
        il.common_autocloud_verified
    );
    if il.other_count > 0 {
        println!(
            "  其他（downloading/workshop/shadercache）: {} 项 / {}",
            il.other_count,
            util::human_size(il.other_bytes)
        );
    }
    println!("  —— Top 10（按体积）——");
    for it in il.items.iter().take(10) {
        let mark = match (it.category, it.has_autocloud) {
            (Category::Common, true) => "✅",
            (Category::Common, false) => "⚠️ ",
            _ => "  ",
        };
        let cat = match it.category {
            Category::Common => "",
            Category::Downloading => " [downloading]",
            Category::Workshop => " [workshop]",
            Category::Shadercache => " [shadercache]",
        };
        println!(
            "  {} {:>10}  {:<38} 修改于 {}{}",
            mark,
            util::human_size(it.size_bytes),
            truncate(&it.name, 38),
            util::fmt_epoch(it.last_modified_epoch),
            cat
        );
    }

    // ---- 存档残留 ----
    let sl = &report.save_leftovers;
    println!("\n【存档残留】（userdata 云存档索引）");
    println!(
        "  userdata 共 {} 个 App 记录，总占用 {}",
        sl.total_apps,
        util::human_size(sl.userdata_total_bytes)
    );
    println!(
        "  其中已卸载游戏的残留 : {} 个 App / {}",
        sl.orphan_apps,
        util::human_size(sl.userdata_orphan_bytes)
    );
    println!(
        "  🔴 疑似从未上传云端（syncstate=2 且本地存在）: {} 个 —— 删除前必须备份！",
        sl.red_alert_apps
    );
    println!("  空壳目录（无任何文件）: {} 个", sl.empty_shells);
    println!(
        "  remote\\ 有文件但索引丢失 : {} 个",
        sl.remote_without_index
    );
    println!("  —— 已卸载残留 Top 10（按本地占用）——");
    for rec in sl.records.iter().filter(|r| !r.installed).take(10) {
        let red = if rec.sync == SyncVerdict::NeverUploadedLocalPresent {
            "🔴"
        } else {
            "  "
        };
        println!(
            "  {} {:>10}  AppID {:<9} {:<20} {}",
            red,
            util::human_size(rec.userdata_bytes),
            rec.appid,
            rec.name.as_deref().unwrap_or("(未知名称)"),
            rec.sync.label()
        );
    }

    // ---- 启发式（当前版本未启用时跳过）----
    let h = &report.heuristic;
    let heuristic_enabled =
        h.stats_bin_appids > 0 || !h.confirmed.is_empty() || !h.suspected.is_empty() || !h.unknown.is_empty();
    if heuristic_enabled {
    println!("\n【启发式扫描】（C 盘散落存档 / 黄金标记 / 痕迹）");
    let new_total = h.confirmed_new + h.suspected_new + h.unknown_new;
    println!(
        "  新发现（未被云索引覆盖）: {} 个目录 / {}（✅确认 {} · ⚠️疑似 {} · ❓未知 {}）",
        new_total,
        util::human_size(h.new_total_bytes),
        h.confirmed_new,
        h.suspected_new,
        h.unknown_new
    );
    println!(
        "  与云索引重叠（已在存档表中）: {} 个；灰名单剪枝: {} 个子树",
        h.confirmed.iter().filter(|d| d.covered_by_cloud_index).count()
            + h.suspected.iter().filter(|d| d.covered_by_cloud_index).count()
            + h.unknown.iter().filter(|d| d.covered_by_cloud_index).count(),
        h.graylisted_pruned
    );
    println!(
        "  stats bin 痕迹: {} 个 App（其中已卸载 {} 个）",
        h.stats_bin_appids, h.stats_bin_orphan_appids
    );
    println!("  —— 新发现 Top 10（按体积）——");
    for d in h
        .confirmed
        .iter()
        .chain(h.suspected.iter())
        .chain(h.unknown.iter())
        .filter(|d| !d.covered_by_cloud_index)
        .take(10)
    {
        let mark = match d.tier {
            Tier::Confirmed => "✅",
            Tier::Suspected => "⚠️ ",
            Tier::Unknown => "❓",
        };
        println!(
            "  {} {:>10}  {}",
            mark,
            util::human_size(d.size_bytes),
            d.path.display()
        );
    }
    } // end if heuristic_enabled

    // ---- 基准对照（基准数字均为主账号口径，此处取记录数最多的账号对齐）----
    let mut main_apps = 0usize;
    let mut main_orphans = 0usize;
    {
        use std::collections::BTreeMap;
        let mut by_acc: BTreeMap<u32, (usize, usize)> = BTreeMap::new();
        for r in &sl.records {
            let e = by_acc.entry(r.account_id).or_default();
            e.0 += 1;
            if !r.installed {
                e.1 += 1;
            }
        }
        if let Some((_uid, (apps, orphans))) = by_acc.into_iter().max_by_key(|(_, v)| v.0) {
            main_apps = apps;
            main_orphans = orphans;
        }
    }
    println!("\n【验收基准对照】（基准为主账号口径，允许 ±5 浮动）");
    print_check("已安装游戏", env.installed_games as i64, 22, 0);
    print_check("common 无主文件夹", il.common_count as i64, 150, 5);
    print_check("主账号 App 记录", main_apps as i64, 291, 5);
    print_check("主账号存档残留", main_orphans as i64, 277, 5);
    println!(
        "  userdata 总占用 : {}（全账号合计，基准 ~1.12 GB）",
        util::human_size(sl.userdata_total_bytes)
    );
    println!(
        "  common 残留体积 : {}（基准 40+ GB）",
        util::human_size(il.common_bytes)
    );

    if !report.warnings.is_empty() {
        println!("\n【警告】（已降级跳过，不影响主流程）");
        for w in report.warnings.iter().take(20) {
            println!("  - {}", w);
        }
        if report.warnings.len() > 20 {
            println!("  … 另有 {} 条，详见 JSON 报告", report.warnings.len() - 20);
        }
    }
    println!();
}

fn print_check(label: &str, actual: i64, expected: i64, tol: i64) {
    let ok = (actual - expected).abs() <= tol;
    println!(
        "  {} {:<18} : {}（基准 {}，{}）",
        if ok { "✅" } else { "❌" },
        label,
        actual,
        expected,
        if ok { "吻合" } else { "偏差" }
    );
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max - 1).collect();
        format!("{}…", t)
    }
}
