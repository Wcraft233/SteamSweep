//! manifest：解析所有库中的 appmanifest_*.acf，得到"已安装游戏"全集。

use crate::vdf;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize)]
pub struct InstalledApp {
    pub appid: u32,
    pub name: String,
    pub installdir: String,
    pub size_on_disk: u64,
    pub library: PathBuf,
}

/// 返回 (appid -> InstalledApp, warnings)
pub fn scan(libraries: &[PathBuf]) -> (BTreeMap<u32, InstalledApp>, Vec<String>) {
    let mut map: BTreeMap<u32, InstalledApp> = BTreeMap::new();
    let mut warnings = Vec::new();

    for lib in libraries {
        let steamapps = lib.join("steamapps");
        let rd = match std::fs::read_dir(&steamapps) {
            Ok(r) => r,
            Err(e) => {
                warnings.push(format!("无法读取 {:?}：{}", steamapps, e));
                continue;
            }
        };
        for entry in rd.flatten() {
            let fname = entry.file_name().to_string_lossy().to_string();
            let lower = fname.to_lowercase();
            if !(lower.starts_with("appmanifest_") && lower.ends_with(".acf")) {
                continue;
            }
            let path = entry.path();
            match parse_acf(&path, lib) {
                Ok(Some(app)) => {
                    map.insert(app.appid, app);
                }
                Ok(None) => {} // 非游戏清单（如工具/运行时），跳过
                Err(e) => warnings.push(format!("解析 {:?} 失败：{}（已跳过）", path, e)),
            }
        }
    }
    (map, warnings)
}

fn parse_acf(path: &std::path::Path, lib: &std::path::Path) -> Result<Option<InstalledApp>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let root = vdf::parse(&text)?;
    let state = root.get_obj("AppState").ok_or("缺少 AppState 节点")?;
    let appid = state
        .get_u64("appid")
        .ok_or("缺少 appid")?
        .try_into()
        .map_err(|_| "appid 超出 u32")?;
    let name = state.get_str("name").unwrap_or("").to_string();
    let installdir = state
        .get_str("installdir")
        .ok_or("缺少 installdir")?
        .to_string();
    let size_on_disk = state.get_u64("SizeOnDisk").unwrap_or(0);

    if appid == 0 || installdir.is_empty() {
        return Ok(None);
    }
    Ok(Some(InstalledApp {
        appid,
        name,
        installdir,
        size_on_disk,
        library: lib.to_path_buf(),
    }))
}
