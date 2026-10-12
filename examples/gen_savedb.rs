//! 开发工具：从 steamsweep-report.json 生成 SaveDB 分片（写入 save-db 仓库）
//! 并合并出随发行捆绑的 savedb.json（appid → 名称 + 存档路径后缀，用于离线归属）。
//! 用法：cargo run --example gen_savedb -- [report.json] [save-db 仓库目录] [savedb.json 输出]

use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path;

/// 绝对路径 → (root 编号, 相对路径)。按已知根目录前缀匹配（大小写不敏感）。
fn abs_to_root(abs: &str) -> Option<(u64, String)> {
    let norm = abs.replace('\\', "/").to_lowercase();
    let prof = std::env::var("USERPROFILE").unwrap_or_default().replace('\\', "/").to_lowercase();
    let local = std::env::var("LOCALAPPDATA").unwrap_or_default().replace('\\', "/").to_lowercase();
    let roaming = std::env::var("APPDATA").unwrap_or_default().replace('\\', "/").to_lowercase();
    let candidates: [(u64, String); 6] = [
        (12, format!("{}/appdata/locallow", prof)),
        (9, format!("{}/saved games", prof)),
        (11, format!("{}/documents/steam cloud", prof)),
        (2, format!("{}/documents", prof)),
        (3, local.clone()),
        (6, roaming.clone()),
    ];
    for (root, base) in candidates {
        if !base.is_empty() && norm.starts_with(&(base.clone() + "/")) {
            let rel = norm[base.len() + 1..].to_string();
            return Some((root, rel));
        }
    }
    None
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let rep_path = args.get(1).map(|s| s.as_str()).unwrap_or("steamsweep-report.json");
    let out_repo = args.get(2).map(|s| s.as_str()).unwrap_or("../save-db");
    let merged_out = args.get(3).map(|s| s.as_str()).unwrap_or("savedb.json");

    let rep: Value = serde_json::from_str(
        &std::fs::read_to_string(rep_path).expect("读 report.json 失败"),
    )
    .expect("解析 report.json 失败");
    let records = rep["save_leftovers"]["records"]
        .as_array()
        .expect("records 缺失");

    let mut shard_count = 0usize;
    // merged: appid -> {"n": name?, "p": [路径后缀...]}
    let mut merged: BTreeMap<u32, (Option<String>, Vec<String>)> = BTreeMap::new();

    for rec in records {
        if rec["installed"].as_bool().unwrap_or(false) {
            continue;
        }
        let Some(appid) = rec["appid"].as_u64().map(|v| v as u32) else { continue };
        let name = rec["name"].as_str().map(|s| s.to_string());

        // save_paths：files 的 (root, 父目录) 聚合
        let mut dirs: BTreeMap<(u64, String), u64> = BTreeMap::new();
        if let Some(files) = rec["files"].as_array() {
            for fi in files {
                let Some(root) = fi["root"].as_u64() else { continue };
                let rel = fi["rel_path"].as_str().unwrap_or("").replace('\\', "/");
                let parent = rel.rsplit_once('/').map(|(p, _)| p.to_string()).unwrap_or_default();
                *dirs.entry((root, parent)).or_default() += 1;
            }
        }
        // files 为空的记录（如 appinfo ufs 补定位）：用 local_save_dirs 绝对路径反推 root
        if dirs.is_empty() {
            if let Some(ld) = rec["local_save_dirs"].as_array() {
                for d in ld {
                    let Some(abs) = d.as_str() else { continue };
                    if let Some((root, rel)) = abs_to_root(abs) {
                        *dirs.entry((root, rel)).or_default() += 1;
                    }
                }
            }
        }
        let save_paths: Vec<Value> = dirs
            .iter()
            .map(|((r, d), n)| {
                json!({
                    "root": r,
                    "pattern": if d.is_empty() { "**".to_string() } else { format!("{}/**", d) },
                    "files_seen": n,
                    "verdict": "save",
                })
            })
            .collect();
        if save_paths.is_empty() {
            continue;
        }

        let entry = json!({
            "appid": appid,
            "name": name,
            "installed_at_scan": false,
            "save_paths": save_paths,
            "stats": {"seen": 1, "deleted": 0},
            "source": "steamsweep-coldstart",
        });
        let shard = format!("{}", appid);
        let shard_dir = Path::new(out_repo).join("db").join(&shard[..3.min(shard.len())]);
        std::fs::create_dir_all(&shard_dir).unwrap();
        std::fs::write(
            shard_dir.join(format!("{}.json", appid)),
            serde_json::to_string_pretty(&entry).unwrap(),
        )
        .unwrap();
        shard_count += 1;

        // merged 后缀路径：数字 root → 文本前缀（root 0/1 太泛化，不用于后缀匹配）
        let e = merged.entry(appid).or_insert((name, Vec::new()));
        for (r, d) in dirs.keys() {
            let prefix = match r {
                2 => "documents/",
                3 => "appdata/local/",
                6 => "appdata/roaming/",
                9 => "saved games/",
                11 => "documents/steam cloud/",
                12 => "appdata/locallow/",
                _ => continue,
            };
            let suffix = format!("{}{}", prefix, d.to_lowercase());
            if !e.1.contains(&suffix) {
                e.1.push(suffix);
            }
        }
    }

    let merged_json: Value = merged
        .iter()
        .map(|(appid, (n, p))| {
            (
                appid.to_string(),
                json!({"n": n, "p": p}),
            )
        })
        .collect::<serde_json::Map<String, Value>>()
        .into();
    std::fs::write(merged_out, serde_json::to_string(&merged_json).unwrap()).unwrap();

    println!("shards={} merged_apps={}", shard_count, merged.len());
}
