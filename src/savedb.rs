//! savedb：随发行捆绑的存档路径数据库快照（SaveDB v1，由 tools/gen_savedb 生成）。
//! 用途：离线游戏名补全 + 启发式目录的归属判定（路径后缀匹配）。

use serde::Deserialize;
use std::collections::HashMap;
use std::sync::OnceLock;

#[derive(Debug, Deserialize)]
pub struct Entry {
    pub n: Option<String>,
    #[serde(default)]
    pub p: Vec<String>,
}

static DB: OnceLock<HashMap<u32, Entry>> = OnceLock::new();

fn db() -> &'static HashMap<u32, Entry> {
    DB.get_or_init(|| serde_json::from_str(include_str!("../savedb.json")).unwrap_or_default())
}

/// 离线游戏名（SaveDB 实测记录名）。
pub fn name(appid: u32) -> Option<&'static str> {
    db().get(&appid).and_then(|e| e.n.as_deref())
}

/// 路径后缀归属：规范化（小写、正斜杠）的完整路径是否以某条记录的后缀结尾。
pub fn match_path(normalized_full: &str) -> Option<u32> {
    for (appid, e) in db() {
        for suf in &e.p {
            if !suf.is_empty() && normalized_full.ends_with(suf.as_str()) {
                return Some(*appid);
            }
        }
    }
    None
}
