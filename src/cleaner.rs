//! cleaner：回收站删除 + 操作日志（M2）。
//! 红线：只作用于本地文件，一律进系统回收站（可从回收站恢复）；
//! 云端数据不删、不改、不写。

use serde::Serialize;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct CleanItem {
    pub path: PathBuf,
    pub bytes: u64,
    /// 日志里的可读标签（游戏名或目录名）
    pub label: String,
}

#[derive(Debug, Serialize)]
struct LogEntry {
    path: String,
    label: String,
    bytes: u64,
    /// "recycled" / "failed"
    action: String,
    error: Option<String>,
}

#[derive(Debug, Default)]
pub struct CleanSummary {
    pub recycled: usize,
    pub freed: u64,
    pub failed: Vec<String>,
    pub log_path: Option<PathBuf>,
}

pub fn data_root() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("SteamSweep")
}

/// 执行清理：逐项移入回收站并写操作日志。progress 回调报告进度。
pub fn run_clean(items: Vec<CleanItem>, mut progress: impl FnMut(String)) -> CleanSummary {
    let mut sum = CleanSummary::default();
    let ts = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let mut entries: Vec<LogEntry> = Vec::new();
    let total = items.len();

    for (i, it) in items.iter().enumerate() {
        progress(format!("正在清理 {}/{}：{}", i + 1, total, it.label));
        if !it.path.exists() {
            // 扫描后已被外部移走：不算失败，静默跳过
            continue;
        }
        let mut error: Option<String> = None;
        match trash::delete(&it.path) {
            Ok(_) => {
                sum.recycled += 1;
                sum.freed += it.bytes;
            }
            Err(e) => {
                error = Some(e.to_string());
                sum.failed.push(format!("{}：{}", it.label, e));
            }
        }
        entries.push(LogEntry {
            path: it.path.display().to_string(),
            label: it.label.clone(),
            bytes: it.bytes,
            action: if error.is_some() { "failed".to_string() } else { "recycled".to_string() },
            error,
        });
    }

    // 操作日志：每次清理一条 JSON（删了什么、原路径、时间）
    #[derive(Serialize)]
    struct Log {
        time: String,
        entries: Vec<LogEntry>,
    }
    let log_dir = data_root().join("logs");
    let _ = std::fs::create_dir_all(&log_dir);
    let log_path = log_dir.join(format!("cleanup-{}.json", ts));
    let log = Log {
        time: chrono::Local::now().to_rfc3339(),
        entries,
    };
    if let Ok(s) = serde_json::to_string_pretty(&log) {
        if std::fs::write(&log_path, s).is_ok() {
            sum.log_path = Some(log_path);
        }
    }
    sum
}
