//! SteamSweep M1 —— Steam 残留清理二合一 · 扫描器
//!
//! 默认启动 GUI（小白清理风格：封面大图 + 复选框 + 分组排序，无命令行窗口）。
//! `steamsweep --cli` 进入控制台模式（附加到父控制台输出报告）。
//!
//! 红线：本程序全程只读，不修改、不删除、不移动任何文件。
#![windows_subsystem = "windows"]

#[allow(dead_code)]
mod heuristic; // 共性痕迹扫描：Fast=黄金标记（限常见目录）+ stats bin；Full 为 M3 备用
mod gui;
mod locator;
mod manifest;
mod orphan;
mod report;
mod savemap;
mod util;
mod vdf;

use std::collections::HashSet;
use std::time::Instant;

fn main() {
    let cli_mode = std::env::args().any(|a| a == "--cli");
    if cli_mode {
        attach_parent_console();
        run_cli();
    } else {
        // 单实例互斥：防止开启第二个 GUI 进程（CLI 工具调用不受限）
        if !acquire_single_instance() {
            bring_existing_to_front();
            return;
        }
        if let Err(e) = gui::run() {
            eprintln!("❌ GUI 启动失败：{}", e);
            std::process::exit(1);
        }
    }
}

/// 命名互斥体实现单实例。句柄故意 forget 不关闭，随进程生命周期持有。
#[cfg(windows)]
fn acquire_single_instance() -> bool {
    use std::os::windows::ffi::OsStrExt;
    extern "system" {
        fn CreateMutexW(attrs: *mut core::ffi::c_void, initial_owner: i32, name: *const u16) -> isize;
        fn GetLastError() -> u32;
        fn CloseHandle(handle: isize) -> i32;
    }
    const ERROR_ALREADY_EXISTS: u32 = 183;
    let name: Vec<u16> = std::ffi::OsStr::new("Local\\SteamSweep_SingleInstance")
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let h = unsafe { CreateMutexW(std::ptr::null_mut(), 1, name.as_ptr()) };
    if h == 0 {
        return true; // 创建失败不阻塞用户
    }
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        unsafe {
            CloseHandle(h);
        }
        return false;
    }
    // 无需持有/关闭句柄：互斥体由内核随进程生命周期管理，进程退出自动释放
    let _ = h;
    true
}

#[cfg(not(windows))]
fn acquire_single_instance() -> bool {
    true
}

/// 找到已运行实例的窗口并前置（恢复最小化 + 置前）。
#[cfg(windows)]
fn bring_existing_to_front() {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "user32")]
    extern "system" {
        fn FindWindowW(class: *const u16, title: *const u16) -> isize;
        fn ShowWindow(hwnd: isize, cmd: i32) -> i32;
        fn SetForegroundWindow(hwnd: isize) -> i32;
        fn IsIconic(hwnd: isize) -> i32;
    }
    const SW_RESTORE: i32 = 9;
    let title: Vec<u16> = std::ffi::OsStr::new(gui::APP_TITLE)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let hwnd = unsafe { FindWindowW(std::ptr::null(), title.as_ptr()) };
    if hwnd != 0 {
        unsafe {
            if IsIconic(hwnd) != 0 {
                ShowWindow(hwnd, SW_RESTORE);
            }
            SetForegroundWindow(hwnd);
        }
    }
}

#[cfg(not(windows))]
fn bring_existing_to_front() {}

/// GUI 子系统程序没有控制台；--cli 模式时附加到父控制台以输出报告。
#[cfg(windows)]
fn attach_parent_console() {
    extern "system" {
        fn AttachConsole(pid: u32) -> i32;
    }
    const ATTACH_PARENT_PROCESS: u32 = 0xFFFFFFFF;
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

#[cfg(not(windows))]
fn attach_parent_console() {}

fn run_cli() {
    println!("SteamSweep M1 · 只读扫描器（本工具不修改任何文件）");
    let t0 = Instant::now();
    let mut warnings: Vec<String> = Vec::new();

    // 1) locator：注册表 → SteamPath → 库 → 账号
    let loc = match locator::locate() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("❌ 定位失败：{}", e);
            std::process::exit(1);
        }
    };
    println!(
        "[1/5] 定位完成：{} 个库，{} 个账号（{:.1?}）",
        loc.libraries.len(),
        loc.accounts.len(),
        t0.elapsed()
    );
    warnings.extend(loc.warnings.clone());

    // 2) manifest：acf → 已安装游戏
    let (installed, w) = manifest::scan(&loc.libraries);
    warnings.extend(w);
    println!("[2/5] 已安装游戏 {} 个（{:.1?}）", installed.len(), t0.elapsed());

    // 3) orphan：common / downloading / workshop / shadercache diff
    let (orphans, w) = orphan::scan(&loc.libraries, &installed, None);
    warnings.extend(w);
    println!(
        "[3/5] 安装残留：common {} 个 / {}，其他 {} 项 / {}（{:.1?}）",
        orphans.common_count,
        util::human_size(orphans.common_bytes),
        orphans.other_count,
        util::human_size(orphans.other_bytes),
        t0.elapsed()
    );

    // 4) savemap：remotecache.vdf + Root 映射 + syncstate
    let (saves, w) = savemap::scan(&loc.steam_path, &loc.accounts, &installed, None);
    warnings.extend(w);
    println!(
        "[4/5] 存档索引：{} 个 App，残留 {} 个，🔴 未上传风险 {} 个（{:.1?}）",
        saves.total_apps,
        saves.orphan_apps,
        saves.red_alert_apps,
        t0.elapsed()
    );

    // 5) 快速痕迹扫描：黄金标记（限常见目录）+ stats bin
    let installed_ids: HashSet<u32> = installed.keys().copied().collect();
    let mut covered: HashSet<String> = HashSet::new();
    for rec in &saves.records {
        for d in &rec.local_save_dirs {
            covered.insert(util::normalize_path(d));
        }
    }
    let (heur, w) = heuristic::scan_fast(&loc.steam_path, &installed_ids, &covered, None);
    warnings.extend(w);
    println!(
        "[5/5] 痕迹扫描：黄金标记 {} 个目录（新发现 {} 个），stats bin {} 个 App（{:.1?}）",
        heur.confirmed.len(),
        heur.confirmed_new,
        heur.stats_bin_appids,
        t0.elapsed()
    );

    // 汇总报告
    let rep = report::build(&loc, &installed, orphans, saves, heur, warnings);
    report::print_summary(&rep);
    util::save_size_cache();

    let json_path = std::path::PathBuf::from("steamsweep-report.json");
    match report::write_json(&rep, &json_path) {
        Ok(()) => println!("JSON 报告已写入：{}", json_path.display()),
        Err(e) => eprintln!("⚠️  {}", e),
    }
    println!("总耗时：{:.1?}", t0.elapsed());
}
