//! locator：自动发现 Steam 安装路径、全部库文件夹、全部账号。全程零用户输入。

use crate::util;
use crate::vdf::{self, Value};
use serde::Serialize;
use std::path::{Path, PathBuf};
use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
use winreg::RegKey;

/// SteamID64 → userdata 账号 ID 的固定偏移。
const STEAMID64_BASE: u64 = 76561197960265728;

#[derive(Debug, Clone, Serialize)]
pub struct Account {
    pub steam_id64: u64,
    pub account_id: u32,
    pub persona: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Location {
    pub steam_path: PathBuf,
    pub libraries: Vec<PathBuf>,
    pub accounts: Vec<Account>,
    #[serde(skip)]
    pub warnings: Vec<String>,
}

pub fn locate() -> Result<Location, String> {
    let mut warnings = Vec::new();

    let steam_path = match find_steam_path() {
        Some(p) => p,
        None => return Err("无法定位 Steam 安装路径（注册表与常见路径均未命中）".to_string()),
    };

    let libraries = parse_libraries(&steam_path, &mut warnings);
    let accounts = parse_accounts(&steam_path, &mut warnings);

    if accounts.is_empty() {
        warnings.push("未发现任何 Steam 账号（userdata 为空）".to_string());
    }

    Ok(Location {
        steam_path,
        libraries,
        accounts,
        warnings,
    })
}

fn find_steam_path() -> Option<PathBuf> {
    // 首选：HKCU\Software\Valve\Steam → SteamPath
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    if let Ok(key) = hkcu.open_subkey(r"Software\Valve\Steam") {
        if let Ok(p) = key.get_value::<String, _>("SteamPath") {
            let pb = PathBuf::from(&p);
            if pb.exists() {
                return Some(pb);
            }
        }
    }
    // 备选：HKLM（32/64 位视图都试）
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    for sub in [r"SOFTWARE\WOW6432Node\Valve\Steam", r"SOFTWARE\Valve\Steam"] {
        if let Ok(key) = hklm.open_subkey(sub) {
            for name in ["InstallPath", "SteamPath"] {
                if let Ok(p) = key.get_value::<String, _>(name) {
                    let pb = PathBuf::from(&p);
                    if pb.exists() {
                        return Some(pb);
                    }
                }
            }
        }
    }
    // 兜底：常见安装位置
    for cand in [r"C:\Program Files (x86)\Steam", r"C:\Program Files\Steam"] {
        let pb = PathBuf::from(cand);
        if pb.join("steamapps").exists() {
            return Some(pb);
        }
    }
    None
}

/// 解析 steamapps\libraryfolders.vdf，返回全部库路径（含 Steam 本体）。
fn parse_libraries(steam_path: &Path, warnings: &mut Vec<String>) -> Vec<PathBuf> {
    let mut libs: Vec<PathBuf> = vec![steam_path.to_path_buf()];
    let vdf_path = steam_path.join("steamapps").join("libraryfolders.vdf");
    let text = match std::fs::read_to_string(&vdf_path) {
        Ok(t) => t,
        Err(e) => {
            warnings.push(format!("读取 {:?} 失败：{}，仅使用 Steam 本体库", vdf_path, e));
            return libs;
        }
    };
    let root = match vdf::parse(&text) {
        Ok(r) => r,
        Err(e) => {
            warnings.push(format!("解析 {:?} 失败：{}，仅使用 Steam 本体库", vdf_path, e));
            return libs;
        }
    };

    let folders = root
        .get_obj("libraryfolders")
        .cloned()
        .unwrap_or_else(|| root.clone());

    for (_key, val) in &folders.entries {
        // 新格式： "0" { "path" "..." } ；旧格式： "0" "D:\\SteamLibrary"
        let path_str = match val {
            Value::Obj(n) => n.get_str("path").map(|s| s.to_string()),
            Value::Str(s) => {
                if s.contains('\\') || s.contains('/') || s.contains(':') {
                    Some(s.clone())
                } else {
                    None
                }
            }
        };
        if let Some(ps) = path_str {
            let pb = PathBuf::from(ps);
            if pb.exists()
                && !libs
                    .iter()
                    .any(|l| util::normalize_path(l) == util::normalize_path(&pb))
            {
                libs.push(pb);
            }
        }
    }
    libs
}

/// 账号发现：loginusers.vdf（拿昵称）∪ userdata 数字目录（保证全量）。
fn parse_accounts(steam_path: &Path, warnings: &mut Vec<String>) -> Vec<Account> {
    let mut accounts: Vec<Account> = Vec::new();

    // 1) config\loginusers.vdf
    let loginusers = steam_path.join("config").join("loginusers.vdf");
    if let Ok(text) = std::fs::read_to_string(&loginusers) {
        match vdf::parse(&text) {
            Ok(root) => {
                if let Some(users) = root.get_obj("users") {
                    for (sid, val) in &users.entries {
                        if let Ok(sid64) = sid.trim().parse::<u64>() {
                            if sid64 > STEAMID64_BASE {
                                let persona = match val {
                                    Value::Obj(n) => n
                                        .get_str("PersonaName")
                                        .map(|s| s.to_string())
                                        .or_else(|| n.get_str("AccountName").map(|s| s.to_string())),
                                    _ => None,
                                };
                                accounts.push(Account {
                                    steam_id64: sid64,
                                    account_id: (sid64 - STEAMID64_BASE) as u32,
                                    persona,
                                });
                            }
                        }
                    }
                }
            }
            Err(e) => warnings.push(format!("解析 {:?} 失败：{}", loginusers, e)),
        }
    }

    // 2) userdata\<数字> 目录兜底（覆盖 loginusers 缺失的账号）
    let userdata = steam_path.join("userdata");
    if let Ok(rd) = std::fs::read_dir(&userdata) {
        for entry in rd.flatten() {
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if !is_dir {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if let Ok(uid) = name.parse::<u32>() {
                if uid > 0 && !accounts.iter().any(|a| a.account_id == uid) {
                    accounts.push(Account {
                        steam_id64: STEAMID64_BASE + uid as u64,
                        account_id: uid,
                        persona: None,
                    });
                }
            }
        }
    }

    accounts.sort_by_key(|a| a.account_id);
    accounts
}
