//! appinfo：解析 `appcache\appinfo.vdf`（二进制 KeyValues，v29 字符串池格式），
//! 提取每个 App 的 `config/ufs/saveblocks`（Steam Auto-Cloud 官方存档配置）。
//! 用途：remotecache.vdf 索引丢失的已卸载游戏，可用 ufs 配置补定位存档目录。
//!
//! 格式要点（v29，magic = 0x07564429）：
//! - 条目：appid u32（0 终止）、size u32（= size 字段之后的字节数）、固定头（约 60 字节，
//!   不同条目可能有数字节差异，故对 blob 起点做暴力探测）、二进制 KV blob；
//! - blob 内字段名是字符串池的 i32 索引，字符串值仍为内联 cstr；
//! - 字符串池在全部条目之后：count u32 + count 个 cstr。
//! 防御式解析：任何一步失败即返回已解析部分，绝不 panic。

use std::path::Path;

#[derive(Debug, Clone)]
pub struct UfsBlock {
    pub appid: u32,
    /// 命名 root（如 "WinSavedGames"、"WinAppDataLocal"、"GameInstall"）
    pub root: String,
    /// 相对 root 的子路径（可能含占位符，调用方过滤）
    pub path: String,
}

struct Reader<'a> {
    d: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn u8(&mut self) -> Option<u8> {
        let b = *self.d.get(self.pos)?;
        self.pos += 1;
        Some(b)
    }
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.pos.checked_add(n)? > self.d.len() {
            return None;
        }
        let s = &self.d[self.pos..self.pos + n];
        self.pos += n;
        Some(s)
    }
    fn u32(&mut self) -> Option<u32> {
        let s = self.take(4)?;
        Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }
    fn i32(&mut self) -> Option<i32> {
        Some(self.u32()? as i32)
    }
    fn u64(&mut self) -> Option<u64> {
        let s = self.take(8)?;
        let mut b = [0u8; 8];
        b.copy_from_slice(s);
        Some(u64::from_le_bytes(b))
    }
    fn cstr(&mut self) -> Option<String> {
        let start = self.pos;
        let mut end = start;
        while end < self.d.len() && self.d[end] != 0 {
            end += 1;
        }
        if end >= self.d.len() {
            return None;
        }
        self.pos = end + 1;
        Some(String::from_utf8_lossy(&self.d[start..end]).to_string())
    }
}

#[derive(Debug)]
enum BVal {
    Str(String),
    Int(i64),
    Obj(Vec<(u32, BVal)>),
}

impl BVal {
    fn obj(&self) -> Option<&Vec<(u32, BVal)>> {
        match self {
            BVal::Obj(v) => Some(v),
            _ => None,
        }
    }
}

/// 读取一个 KV 对象（字段名为字符串池索引），直到 0x08 结束标记。
/// 返回 (字段列表, 消耗后的 pos 是否仍在 data 内)。
fn read_kv(r: &mut Reader, end_limit: usize, depth: u32) -> Option<Vec<(u32, BVal)>> {
    if depth > 24 || r.pos >= end_limit {
        return None;
    }
    let mut items = Vec::new();
    loop {
        if r.pos >= end_limit {
            return None; // 没见到结束标记就出了界：起点猜错了
        }
        let ty = r.u8()?;
        match ty {
            0x00 => {
                let idx = r.u32()?;
                let sub = read_kv(r, end_limit, depth + 1)?;
                items.push((idx, BVal::Obj(sub)));
            }
            0x01 => {
                let idx = r.u32()?;
                let v = r.cstr()?;
                items.push((idx, BVal::Str(v)));
            }
            0x02 => {
                let idx = r.u32()?;
                let v = r.i32()?;
                items.push((idx, BVal::Int(v as i64)));
            }
            0x07 => {
                let idx = r.u32()?;
                let v = r.u64()?;
                items.push((idx, BVal::Int(v as i64)));
            }
            0x03 => {
                let idx = r.u32()?;
                r.take(4)?;
                items.push((idx, BVal::Int(0)));
            }
            0x08 => return Some(items),
            _ => return None,
        }
        if items.len() > 100_000 {
            return None;
        }
    }
}

/// 在 entry_end 之前探测 blob 起点：固定头长度因条目而异（约 56~72 字节），
/// 逐个候选偏移试解析，第一个"解析成功且终点不超界"的即为真起点。
fn parse_blob(d: &[u8], after_size: usize, entry_end: usize) -> Option<Vec<(u32, BVal)>> {
    for hs in (56usize..=76).step_by(4) {
        let start = after_size.checked_add(hs)?;
        if start >= entry_end || start >= d.len() {
            continue;
        }
        let mut r = Reader { d, pos: start };
        // blob 顶层是一个 0x00 对象
        if r.u8() != Some(0x00) {
            continue;
        }
        if r.u32().is_none() {
            continue; // 顶层对象名（池索引）
        }
        if let Some(items) = read_kv(&mut r, entry_end, 0) {
            return Some(items);
        }
    }
    None
}

fn find<'a>(items: &'a [(u32, BVal)], pool: &[String], key: &str) -> Option<&'a BVal> {
    items
        .iter()
        .find(|(idx, _)| pool.get(*idx as usize).map(|s| s.eq_ignore_ascii_case(key)).unwrap_or(false))
        .map(|(_, v)| v)
}

fn extract_ufs(appid: u32, root_items: &[(u32, BVal)], pool: &[String], out: &mut Vec<UfsBlock>) {
    let appinfo = find(root_items, pool, "appinfo")
        .and_then(|v| v.obj())
        .map(|o| o.as_slice())
        .unwrap_or(root_items);
    let Some(ufs) = find(appinfo, pool, "ufs").and_then(|v| v.obj()) else {
        return;
    };
    // ufs 下的存档块键名：新版叫 savefiles，旧版叫 saveblocks，都试
    let blocks = find(ufs, pool, "savefiles")
        .and_then(|v| v.obj())
        .or_else(|| find(ufs, pool, "saveblocks").and_then(|v| v.obj()));
    let Some(blocks) = blocks else {
        return;
    };
    for (_, bv) in blocks {
        let Some(block) = bv.obj() else { continue };
        // root 是命名字符串（"WinSavedGames" 等），兼容偶发的数字形式
        let root = match find(block, pool, "root") {
            Some(BVal::Str(s)) => s.clone(),
            Some(BVal::Int(i)) => i.to_string(),
            _ => continue,
        };
        let path = match find(block, pool, "path") {
            Some(BVal::Str(s)) => s.clone(),
            _ => String::new(),
        };
        out.push(UfsBlock { appid, root, path });
    }
}

/// 解析 appinfo.vdf，返回全部 ufs saveblocks。失败返回空 Vec。
pub fn parse_ufs(steam_path: &Path) -> Vec<UfsBlock> {
    let (out, _, _) = parse_inner(steam_path);
    out
}

fn parse_inner(steam_path: &Path) -> (Vec<UfsBlock>, usize, Vec<String>) {
    let file = steam_path.join("appcache").join("appinfo.vdf");
    let data = match std::fs::read(&file) {
        Ok(d) => d,
        Err(_) => return (Vec::new(), 0, Vec::new()),
    };
    let mut out = Vec::new();
    let mut r = Reader { d: &data, pos: 0 };
    if r.u32().unwrap_or(0) != 0x0756_4429 {
        return (out, 0, Vec::new()); // 只支持 v29（2018 年后的 Steam 都是这个版本）
    }
    let _universe = r.u32();

    // ---- 第一遍：走条目，解析 blob（字段名先记池索引）----
    struct Entry {
        appid: u32,
        items: Vec<(u32, BVal)>,
    }
    let mut entries: Vec<Entry> = Vec::new();
    let mut entries_end = r.pos;
    for _ in 0..500_000 {
        let Some(appid) = r.u32() else { break };
        if appid == 0 {
            entries_end = r.pos;
            break;
        }
        let Some(size) = r.u32() else { break };
        let after_size = r.pos;
        let entry_end = after_size.saturating_add(size as usize).min(data.len());
        if size == 0 {
            continue; // 墓碑条目
        }
        if let Some(items) = parse_blob(&data, after_size, entry_end) {
            entries.push(Entry { appid, items });
        }
        r.pos = entry_end;
        entries_end = entry_end;
    }

    // ---- 字符串池：条目区结束后 count u32 + cstr×count ----
    let mut pool: Vec<String> = Vec::new();
    let mut pr = Reader { d: &data, pos: entries_end };
    if let Some(count) = pr.u32() {
        if (count as usize) <= 5_000_000 {
            for _ in 0..count {
                match pr.cstr() {
                    Some(s) => pool.push(s),
                    None => break,
                }
            }
        }
    }

    for e in &entries {
        extract_ufs(e.appid, &e.items, &pool, &mut out);
    }
    let n = entries.len();
    (out, n, pool)
}
