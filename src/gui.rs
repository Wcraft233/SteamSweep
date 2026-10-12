//! GUI：eframe/egui 原生界面 —— 小白清理工具风格。
//! 大封面图 + 大字游戏名 + 复选框勾选 + 按大小排序 + 分组呈现，无任何技术黑话。
//! 扫描在后台线程分阶段执行，结果流式填充；封面图后台下载并缓存到本地。
//! 全程只读：不修改、不删除、不移动任何文件（勾选状态为 M2 清理做准备）。

use crate::cleaner::{self, CleanItem, CleanSummary};
use crate::heuristic::{self, HeuristicDir};
use crate::locator::{self, Location};
use crate::manifest;
use crate::orphan::{self, Category, OrphanDir};
use crate::savemap::{self, AppSaveRecord, SyncVerdict};
use crate::util;
use eframe::egui::{self, Align2, Color32, FontId, Rect, RichText, Sense, Stroke, TextureHandle, Vec2};
use egui_extras::{Column, TableBuilder};
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

const GREEN: Color32 = Color32::from_rgb(90, 210, 130);
const YELLOW: Color32 = Color32::from_rgb(240, 195, 80);
const RED: Color32 = Color32::from_rgb(240, 100, 100);
const GRAY: Color32 = Color32::from_rgb(143, 152, 160);
const BLUE: Color32 = Color32::from_rgb(102, 192, 244);
// Steam 风深色主题
const ACCENT: Color32 = Color32::from_rgb(102, 192, 244);
const BG_DEEP: Color32 = Color32::from_rgb(23, 26, 33);
const BG_PANEL: Color32 = Color32::from_rgb(27, 40, 56);
const BG_CARD: Color32 = Color32::from_rgb(36, 52, 70);
const NAV_SELECTED: Color32 = Color32::from_rgb(42, 66, 92);

/// 内嵌美术资产（AI 生成，构建期打包进 exe）
const LOGO_PNG: &[u8] = include_bytes!("../assets/logo_256.png");
const EMPTY_STATE_PNG: &[u8] = include_bytes!("../assets/empty_state_512.png");
const COVER_PLACEHOLDER_JPG: &[u8] = include_bytes!("../assets/cover_placeholder_640.jpg");

const COVER_W: f32 = 110.0;
const COVER_H: f32 = 52.0;
const ROW_H: f32 = 62.0;
const NAV_W: f32 = 200.0;

enum Msg {
    Stage(String),
    Env { loc: Location, installed: usize },
    OrphanItem(OrphanDir),
    OrphanTotals { common_count: usize, common_bytes: u64, verified: usize, other_count: usize, other_bytes: u64 },
    SaveItem(AppSaveRecord),
    SaveTotals { total_apps: usize, orphan_apps: usize, total_bytes: u64, orphan_bytes: u64, red: usize, empty: usize, rwoi: usize },
    TraceItem(HeuristicDir),
    TraceTotals { confirmed: usize, new_dirs: usize, new_bytes: u64, graylist: usize, stats_bin: usize, stats_orphan: usize },
    Cover { appid: u32, img: egui::ColorImage },
    CoverFailed(u32),
    /// display = 展示名（中文优先）；en = 英文名（用于文件夹名反查，仅当与 display 不同时存在）
    Name { appid: u32, display: String, en: Option<String> },
    /// IStoreService/GetAppList 全量英文名表（机会型拉取，用于文件夹名反查）
    NamesBulk(Vec<(u32, String)>),
    /// 清理进度（M2）
    CleanProgress(String),
    CleanDone(CleanSummary),
    /// 卸载事件（M4 watcher）
    Uninstalled { appid: u32, name: String },
    Warning(String),
    Error(String),
    Done(f64),
}

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Installs,
    Saves,
    Traces,
}

enum CoverState {
    Loading,
    Ready(TextureHandle),
    Failed,
}

pub const APP_TITLE: &str = "SteamSweep — Steam 残留清理";

pub fn run() -> eframe::Result<()> {
    let mut viewport = egui::ViewportBuilder::default()
        .with_title(APP_TITLE)
        .with_inner_size([1180.0, 800.0])
        .with_min_inner_size([1000.0, 640.0]);
    if let Some(icon) = load_icon() {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "SteamSweep",
        options,
        Box::new(|cc| Ok(Box::new(SteamSweepApp::new(cc)))),
    )
}

struct SteamSweepApp {
    scanning: bool,
    stage: String,
    rx: Option<mpsc::Receiver<Msg>>,
    started_at: Option<Instant>,
    elapsed: Option<f64>,

    env: Option<(Location, usize)>,
    orphans: Vec<OrphanDir>,
    orphan_totals: Option<(usize, u64, usize, usize, u64)>,
    saves: Vec<AppSaveRecord>,
    save_totals: Option<(usize, usize, u64, u64, usize, usize, usize)>,
    traces: Vec<HeuristicDir>,
    trace_totals: Option<(usize, usize, u64, usize, usize, usize)>,
    warnings: Vec<String>,

    covers: HashMap<u32, CoverState>,
    /// AppID → 游戏展示名（中文优先，商店页抓取，持久化到 names.json）
    names: HashMap<u32, String>,
    /// AppID → 英文名（仅当与展示名不同；反查文件夹名用，持久化到 names_en.json）
    names_en: HashMap<u32, String>,
    names_dirty: bool,
    /// 小写名字 → AppID 反向索引（names_en ∪ names 重建；全量表有十几万条，反查必须走哈希）
    names_rev: HashMap<String, u32>,
    /// 上次名称缓存落盘时间（防抖：全量表十几MB，不能每帧序列化）
    names_saved_at: Option<Instant>,
    /// 本次运行是否已启动全量名称表拉取（防重复启动重复拉）
    applist_started: bool,
    /// account_id → 头像纹理（None = 查过但没有）
    avatars: HashMap<u32, Option<TextureHandle>>,

    sel_installs: HashSet<String>,
    sel_saves: HashSet<String>,
    /// 折叠中的存档分组（默认 StillInstalled 折叠）
    collapsed_groups: HashSet<SaveGroup>,

    tab: Tab,
    saves_filter: String,
    dirty_sort: bool,

    // 设置窗口
    settings_open: bool,
    proxy_input: String,
    apikey_input: String,
    dirs_input: String,

    /// 是否显示 Steam 运行时组件（Proton / Steam Linux Runtime / Steamworks 运行库等）。
    /// 默认排除：它们不是游戏，删了 Steam 也会自动重装。
    show_runtime: bool,

    // 清理流程（M2）
    confirm_open: bool,
    cleaning: bool,
    clean_progress: String,
    clean_result: Option<CleanSummary>,
    result_open: bool,

    // 卸载联动提醒（M4）
    pending_uninstalls: Vec<(u32, String)>,
    banner: Option<UninstallBanner>,

    // 内嵌美术资产纹理（M5 界面改版）
    logo_tex: Option<TextureHandle>,
    empty_tex: Option<TextureHandle>,
    placeholder_tex: Option<TextureHandle>,
}

/// 卸载联动横幅（M4）
#[derive(Clone)]
struct UninstallBanner {
    appid: u32,
    name: String,
    save_count: usize,
    save_bytes: u64,
    orphan_count: usize,
    orphan_bytes: u64,
    has_red: bool,
}

impl SteamSweepApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        install_cjk_fonts(&cc.egui_ctx);
        // egui 0.31 默认跟随系统主题（本机是浅色），必须显式锁死深色
        cc.egui_ctx.set_theme(egui::ThemePreference::Dark);
        // Steam 风深色主题
        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = BG_DEEP;
        visuals.window_fill = BG_PANEL;
        visuals.extreme_bg_color = Color32::from_rgb(17, 20, 26);
        visuals.faint_bg_color = Color32::from_rgb(30, 42, 58);
        visuals.widgets.inactive.bg_fill = Color32::from_rgb(48, 66, 88);
        visuals.widgets.inactive.weak_bg_fill = Color32::from_rgb(38, 53, 72);
        visuals.widgets.hovered.bg_fill = Color32::from_rgb(62, 84, 110);
        visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(50, 69, 92);
        visuals.widgets.active.bg_fill = Color32::from_rgb(72, 98, 128);
        visuals.widgets.active.weak_bg_fill = Color32::from_rgb(60, 82, 108);
        visuals.selection.bg_fill = Color32::from_rgb(52, 96, 130);
        visuals.hyperlink_color = ACCENT;
        cc.egui_ctx.set_visuals(visuals);
        let mut style = (*cc.egui_ctx.style()).clone();
        style.text_styles.insert(egui::TextStyle::Body, FontId::proportional(15.0));
        style.text_styles.insert(egui::TextStyle::Heading, FontId::proportional(24.0));
        style.text_styles.insert(egui::TextStyle::Button, FontId::proportional(14.0));
        style.text_styles.insert(egui::TextStyle::Small, FontId::proportional(12.0));
        style.spacing.button_padding = Vec2::new(12.0, 6.0);
        cc.egui_ctx.set_style(style);

        // 内嵌美术资产 → 纹理（logo / 空状态插画 / 封面占位图）
        let logo_tex = load_tex(&cc.egui_ctx, "logo", LOGO_PNG);
        let empty_tex = load_tex(&cc.egui_ctx, "empty-state", EMPTY_STATE_PNG);
        let placeholder_tex = load_tex(&cc.egui_ctx, "cover-placeholder", COVER_PLACEHOLDER_JPG);

        let mut names = load_name_cache();
        let mut names_en = load_name_en_cache();
        if names_en.is_empty() && !names.is_empty() {
            // 旧版缓存全是英文名：整体转为反查表，展示名重新抓（中文优先）
            names_en = names.clone();
            names.clear();
        }
        let names_rev = build_names_rev(&names_en, &names);

        let mut app = Self {
            scanning: false,
            stage: "就绪".to_string(),
            rx: None,
            started_at: None,
            elapsed: None,
            env: None,
            orphans: Vec::new(),
            orphan_totals: None,
            saves: Vec::new(),
            save_totals: None,
            traces: Vec::new(),
            trace_totals: None,
            warnings: Vec::new(),
            covers: HashMap::new(),
            names,
            names_en,
            names_dirty: false,
            names_rev,
            names_saved_at: None,
            applist_started: false,
            avatars: HashMap::new(),
            sel_installs: HashSet::new(),
            sel_saves: HashSet::new(),
            collapsed_groups: HashSet::from([SaveGroup::StillInstalled]),
            tab: match std::env::var("STEAMSWEEP_TAB").as_deref() {
                Ok("saves") => Tab::Saves,
                Ok("traces") => Tab::Traces,
                _ => Tab::Installs,
            },
            saves_filter: String::new(),
            dirty_sort: false,
            settings_open: false,
            proxy_input: String::new(),
            apikey_input: String::new(),
            dirs_input: String::new(),
            show_runtime: false,
            confirm_open: false,
            cleaning: false,
            clean_progress: String::new(),
            clean_result: None,
            result_open: false,
            pending_uninstalls: Vec::new(),
            banner: None,
            logo_tex,
            empty_tex,
            placeholder_tex,
        };
        // 启动即自动开始逐步扫描（只读，无副作用）
        app.start_scan(&cc.egui_ctx);
        app
    }

    fn start_scan(&mut self, ctx: &egui::Context) {
        if self.scanning {
            return;
        }
        self.scanning = true;
        self.stage = "正在定位 Steam…".to_string();
        self.started_at = Some(Instant::now());
        self.elapsed = None;
        self.env = None;
        self.orphans.clear();
        self.orphan_totals = None;
        self.saves.clear();
        self.save_totals = None;
        self.traces.clear();
        self.trace_totals = None;
        self.warnings.clear();
        self.covers.clear();
        self.sel_installs.clear();
        self.sel_saves.clear();
        self.dirty_sort = false;

        let (tx, rx) = mpsc::sync_channel::<Msg>(4096);
        self.rx = Some(rx);
        let tx_scan = tx.clone();
        let ctx2 = ctx.clone();
        std::thread::spawn(move || scan_worker(tx_scan, ctx2));

        // 全量英文名表（IStoreService/GetAppList，需 API key）：机会型拉取。
        // api.steampowered.com 在本机被间歇性 SNI 阻断，拉不到就静默放弃，不影响主功能。
        if !self.applist_started {
            if let Some(key) = applist_key_if_stale(self.names_en.len()) {
                self.applist_started = true;
                let tx2 = tx.clone();
                let ctx3 = ctx.clone();
                std::thread::spawn(move || applist_worker(key, tx2, ctx3));
            }
        }
    }

    fn start_cover_downloads(&mut self, ctx: &egui::Context) {
        // 按界面展示顺序排列：默认 Tab 是安装残留，其封面优先；其次存档列表（已按大小排好）。
        // 幂等可重入：已在 covers 表里的跳过；名称表后到的条目会在名字补全后再次走到这里。
        let mut ordered: Vec<u32> = Vec::new();
        let mut seen: HashSet<u32> = HashSet::new();
        for it in &self.orphans {
            let aid = it
                .appid_hint
                .or_else(|| self.names_rev.get(&it.name.to_lowercase()).copied());
            if let Some(a) = aid {
                if seen.insert(a) && !self.covers.contains_key(&a) {
                    ordered.push(a);
                }
            }
        }
        for rec in &self.saves {
            if seen.insert(rec.appid) && !self.covers.contains_key(&rec.appid) {
                ordered.push(rec.appid);
            }
        }
        for a in &ordered {
            self.covers.insert(*a, CoverState::Loading);
        }
        if ordered.is_empty() {
            return;
        }
        // 已有名字（acf、内置表或缓存）的不再联网查询
        let mut known: HashSet<u32> = self.names.keys().copied().collect();
        for rec in &self.saves {
            if rec.name.is_some() {
                known.insert(rec.appid);
            }
        }
        let appid_set: HashSet<u32> = ordered.iter().copied().collect();
        let need_name: HashSet<u32> = appid_set
            .difference(&known)
            .copied()
            .filter(|a| well_known_app_name(*a).is_none())
            .collect();
        let steam_path = self.env.as_ref().map(|(loc, _)| loc.steam_path.clone());
        let tx = self.tx_like(ctx);
        spawn_cover_workers(ordered, need_name, steam_path, tx, ctx.clone());
    }

    /// 封面下载完成后需要一个新的 Sender —— 扫描结束后 rx 已断开也无妨，重建一条 channel。
    fn tx_like(&mut self, ctx: &egui::Context) -> mpsc::SyncSender<Msg> {
        let (tx, rx) = mpsc::sync_channel::<Msg>(4096);
        self.rx = Some(rx);
        let _ = ctx;
        tx
    }

    fn drain_messages(&mut self, ctx: &egui::Context) {
        let rx = match self.rx.take() {
            Some(r) => r,
            None => return,
        };
        let mut done = false;
        while let Ok(msg) = rx.try_recv() {
            match msg {
                Msg::Stage(s) => self.stage = s,
                Msg::Env { loc, installed } => self.env = Some((loc, installed)),
                Msg::OrphanItem(it) => {
                    self.orphans.push(it);
                    self.dirty_sort = true;
                }
                Msg::OrphanTotals { common_count, common_bytes, verified, other_count, other_bytes } => {
                    self.orphan_totals = Some((common_count, common_bytes, verified, other_count, other_bytes));
                }
                Msg::SaveItem(rec) => {
                    // 本地已无文件的记录不展示（也省去封面/名称的联网开销）
                    if !rec.is_empty_shell {
                        self.saves.push(rec);
                        self.dirty_sort = true;
                    }
                }
                Msg::SaveTotals { total_apps, orphan_apps, total_bytes, orphan_bytes, red, empty, rwoi } => {
                    self.save_totals = Some((total_apps, orphan_apps, total_bytes, orphan_bytes, red, empty, rwoi));
                }
                Msg::TraceItem(d) => {
                    self.traces.push(d);
                    self.dirty_sort = true;
                }
                Msg::TraceTotals { confirmed, new_dirs, new_bytes, graylist, stats_bin, stats_orphan } => {
                    self.trace_totals = Some((confirmed, new_dirs, new_bytes, graylist, stats_bin, stats_orphan));
                }
                Msg::Cover { appid, img } => {
                    // 解码已在工作线程完成，这里只做纹理上传（快）
                    let tex = ctx.load_texture(
                        format!("cover-{}", appid),
                        img,
                        egui::TextureOptions::LINEAR,
                    );
                    self.covers.insert(appid, CoverState::Ready(tex));
                }
                Msg::CoverFailed(appid) => {
                    self.covers.insert(appid, CoverState::Failed);
                }
                Msg::Name { appid, display, en } => {
                    if let Some(en) = en {
                        self.names_en.insert(appid, en);
                    }
                    self.names.insert(appid, display);
                    self.names_dirty = true;
                }
                Msg::NamesBulk(list) => {
                    // 全量表只补空缺：store 页抓到的/上次缓存的更准（可能是本地化后的名字）
                    for (appid, name) in list {
                        self.names_en.entry(appid).or_insert(name);
                    }
                    self.names_dirty = true;
                }
                Msg::CleanProgress(s) => self.clean_progress = s,
                Msg::Uninstalled { appid, name } => {
                    let muted = load_local_config().muted_appids.unwrap_or_default();
                    if !muted.contains(&appid) {
                        self.pending_uninstalls.push((appid, name));
                        if !self.scanning {
                            // 重扫让残留浮现；横幅在扫描完成时生成
                            self.start_scan(ctx);
                        }
                    }
                }
                Msg::CleanDone(sum) => {
                    self.cleaning = false;
                    self.clean_progress.clear();
                    self.result_open = true;
                    self.clean_result = Some(sum);
                    self.sel_installs.clear();
                    self.sel_saves.clear();
                    // 清完自动重扫，界面与磁盘状态对齐
                    self.start_scan(ctx);
                }
                Msg::Warning(w) => self.warnings.push(w),
                Msg::Error(e) => {
                    self.warnings.push(format!("错误：{}", e));
                    self.scanning = false;
                }
                Msg::Done(secs) => {
                    self.elapsed = Some(secs);
                    self.scanning = false;
                    self.stage = "扫描完成".to_string();
                    done = true;
                    self.build_uninstall_banner();
                }
            }
        }
        // 循环内可能已调用 start_scan / tx_like 重建 channel（如清理完成后的自动重扫、
        // 卸载事件触发的重扫），此时 self.rx 已是新通道，绝不能把旧 rx 盖回去。
        if self.rx.is_none() {
            self.rx = Some(rx);
        }
        if self.names_dirty {
            // 防抖落盘：名称流式到达期间每帧都脏，全量表序列化一次要几十毫秒，最多每 2 秒写一次
            let due = self
                .names_saved_at
                .map(|t| t.elapsed() > Duration::from_secs(2))
                .unwrap_or(true);
            if due {
                save_name_cache(&self.names);
                save_name_en_cache(&self.names_en);
                self.names_rev = build_names_rev(&self.names_en, &self.names);
                self.names_dirty = false;
                self.names_saved_at = Some(Instant::now());
                // 名字补全后，之前反查不到 AppID 的孤儿现在能匹配了 → 补拉封面
                self.start_cover_downloads(ctx);
            } else {
                // 2 秒内再检查一次，保证最后的名称最终落盘
                ctx.request_repaint_after(Duration::from_millis(500));
            }
        }
        if self.dirty_sort && !self.scanning {
            self.orphans.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes));
            self.saves.sort_by(|a, b| b.userdata_bytes.cmp(&a.userdata_bytes));
            self.traces.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes));
            self.dirty_sort = false;
        }
        if done {
            self.start_cover_downloads(ctx);
        }
    }

    fn cover_for(&self, appid: Option<u32>) -> Option<&TextureHandle> {
        appid.and_then(|a| self.covers.get(&a)).and_then(|s| match s {
            CoverState::Ready(t) => Some(t),
            _ => None,
        })
    }

    /// 按文件夹名反查 AppID：走预建的反向哈希索引（全量表十几万条，不能线性扫）。
    fn appid_by_name(&self, name: &str) -> Option<u32> {
        self.names_rev.get(&name.to_lowercase()).copied()
    }

    /// 孤儿目录的封面 AppID：优先 appid_hint，退化为名称反查。
    fn orphan_appid(&self, it: &OrphanDir) -> Option<u32> {
        it.appid_hint.or_else(|| self.appid_by_name(&it.name))
    }

    /// 账号显示名：PersonaName 优先，退化为数字 ID。
    fn account_label(&self, account_id: u32) -> String {
        self.env
            .as_ref()
            .and_then(|(loc, _)| loc.accounts.iter().find(|a| a.account_id == account_id))
            .and_then(|a| a.persona.clone())
            .filter(|p| !p.trim().is_empty())
            .unwrap_or_else(|| account_id.to_string())
    }

    /// 账号头像（懒加载 + 记忆化）：<steam>\config\avatarcache\<steamid64>.{png,jpg,jpeg}
    fn avatar_for(&mut self, ctx: &egui::Context, account_id: u32) -> Option<TextureHandle> {
        if let Some(v) = self.avatars.get(&account_id) {
            return v.clone();
        }
        let tex = self
            .env
            .as_ref()
            .and_then(|(loc, _)| {
                let acct = loc.accounts.iter().find(|a| a.account_id == account_id)?;
                let dir = loc.steam_path.join("config").join("avatarcache");
                for ext in ["png", "jpg", "jpeg"] {
                    let p = dir.join(format!("{}.{}", acct.steam_id64, ext));
                    if let Ok(b) = std::fs::read(&p) {
                        if b.len() > 200 {
                            return Some(b);
                        }
                    }
                }
                None
            })
            .and_then(|b| image::load_from_memory(&b).ok())
            .map(|img| {
                let rgba = img.to_rgba8();
                let size = [rgba.width() as usize, rgba.height() as usize];
                let ci = egui::ColorImage::from_rgba_unmultiplied(size, &rgba);
                ctx.load_texture(format!("avatar-{}", account_id), ci, egui::TextureOptions::LINEAR)
            });
        self.avatars.insert(account_id, tex.clone());
        tex
    }

    fn selected_totals(&self) -> (usize, u64) {
        let mut n = 0usize;
        let mut bytes = 0u64;
        for it in &self.orphans {
            // 隐藏的运行时组件不计入勾选（即使之前勾过）
            if !self.show_runtime && is_runtime_orphan(it) {
                continue;
            }
            if self.sel_installs.contains(&it.path.to_string_lossy().to_string()) {
                n += 1;
                bytes += it.size_bytes;
            }
        }
        for rec in &self.saves {
            if !self.show_runtime && is_runtime_app(rec.appid) {
                continue;
            }
            if self.sel_saves.contains(&save_key(rec)) {
                n += 1;
                bytes += rec.userdata_bytes;
            }
        }
        (n, bytes)
    }

    /// 汇总当前勾选为清理条目（M2）。
    /// 存档条目 = userdata\<uid>\<appid> 整个目录 + 解析出的外部存档目录（排除 userdata 内重叠）。
    fn build_clean_items(&self) -> Vec<CleanItem> {
        let mut items = Vec::new();
        for it in &self.orphans {
            if !self.show_runtime && is_runtime_orphan(it) {
                continue;
            }
            if self.sel_installs.contains(&it.path.to_string_lossy().to_string()) {
                items.push(CleanItem {
                    path: it.path.clone(),
                    bytes: it.size_bytes,
                    label: it.name.clone(),
                });
            }
        }
        let steam = self.env.as_ref().map(|(l, _)| l.steam_path.clone());
        for rec in &self.saves {
            if !self.show_runtime && is_runtime_app(rec.appid) {
                continue;
            }
            if !self.sel_saves.contains(&save_key(rec)) {
                continue;
            }
            let name = rec
                .name
                .clone()
                .or_else(|| self.names.get(&rec.appid).cloned())
                .unwrap_or_else(|| format!("AppID {}", rec.appid));
            if let Some(sp) = &steam {
                let udir = sp
                    .join("userdata")
                    .join(rec.account_id.to_string())
                    .join(rec.appid.to_string());
                if udir.exists() {
                    items.push(CleanItem {
                        path: udir,
                        bytes: rec.userdata_bytes,
                        label: format!("{}（Steam 存档记录）", name),
                    });
                }
            }
            for d in &rec.local_save_dirs {
                // 与 userdata 目录重叠的跳过（整目录已入列）
                if steam
                    .as_ref()
                    .map(|sp| d.starts_with(sp.join("userdata")))
                    .unwrap_or(false)
                {
                    continue;
                }
                let dirname = d
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| d.display().to_string());
                items.push(CleanItem {
                    path: d.clone(),
                    bytes: 0,
                    label: format!("{}（{}）", name, dirname),
                });
            }
        }
        items
    }

    fn start_clean(&mut self, ctx: &egui::Context) {
        let items = self.build_clean_items();
        self.confirm_open = false;
        if items.is_empty() {
            return;
        }
        self.cleaning = true;
        self.clean_progress = "准备清理…".to_string();
        let tx = self.tx_like(ctx);
        let ctx2 = ctx.clone();
        std::thread::spawn(move || {
            let txp = tx.clone();
            let ctxp = ctx2.clone();
            let sum = cleaner::run_clean(items, move |s| {
                let _ = txp.send(Msg::CleanProgress(s));
                ctxp.request_repaint();
            });
            let _ = tx.send(Msg::CleanDone(sum));
            ctx2.request_repaint();
        });
    }

    /// 扫描完成后，从待处理的卸载事件里挑第一个"确有残留"的生成横幅（M4）。
    fn build_uninstall_banner(&mut self) {
        if self.banner.is_some() {
            return;
        }
        let muted = load_local_config().muted_appids.unwrap_or_default();
        while let Some((appid, name)) = self.pending_uninstalls.first().cloned() {
            self.pending_uninstalls.remove(0);
            if muted.contains(&appid) {
                continue;
            }
            let mut save_count = 0usize;
            let mut save_bytes = 0u64;
            let mut has_red = false;
            for r in &self.saves {
                if r.appid == appid {
                    save_count += 1;
                    save_bytes += r.userdata_bytes;
                    if matches!(r.sync, SyncVerdict::NeverUploadedLocalPresent) {
                        has_red = true;
                    }
                }
            }
            let mut oc = 0usize;
            let mut ob = 0u64;
            for it in &self.orphans {
                if it.appid_hint == Some(appid) || it.name.eq_ignore_ascii_case(&name) {
                    oc += 1;
                    ob += it.size_bytes;
                }
            }
            if save_count == 0 && oc == 0 {
                continue; // 没有残留就不打扰
            }
            self.banner = Some(UninstallBanner {
                appid,
                name,
                save_count,
                save_bytes,
                orphan_count: oc,
                orphan_bytes: ob,
                has_red,
            });
            break;
        }
    }

    /// 勾选中的红色存档（疑似从未上传云端）数量
    fn selected_red_saves(&self) -> usize {
        self.saves
            .iter()
            .filter(|r| {
                (self.show_runtime || !is_runtime_app(r.appid))
                    && self.sel_saves.contains(&save_key(r))
                    && matches!(r.sync, SyncVerdict::NeverUploadedLocalPresent)
            })
            .count()
    }

    /// 勾选中属于未卸载游戏的存档数量
    fn selected_installed_saves(&self) -> usize {
        self.saves
            .iter()
            .filter(|r| {
                (self.show_runtime || !is_runtime_app(r.appid))
                    && self.sel_saves.contains(&save_key(r))
                    && r.installed
            })
            .count()
    }
}

fn save_key(rec: &AppSaveRecord) -> String {
    format!("{}:{}", rec.appid, rec.account_id)
}

// ---------- 存档分组（按用户指定的默认排序逻辑） ----------
#[derive(PartialEq, Eq, Hash, Clone, Copy)]
enum SaveGroup {
    /// 已卸载 · 云端有副本（可放心清理）
    CloudSynced,
    /// 已卸载 · 仅本地痕迹（知道是游戏存档，但游戏本体找不到）
    OrphanNoIndex,
    /// 疑似从未上传云端（删除前务必备份）
    NeverSynced,
    /// 未卸载（默认折叠）
    StillInstalled,
}

fn group_of(rec: &AppSaveRecord) -> SaveGroup {
    if rec.installed {
        return SaveGroup::StillInstalled;
    }
    match rec.sync {
        SyncVerdict::NeverUploadedLocalPresent | SyncVerdict::Mixed => SaveGroup::NeverSynced,
        _ => {
            if rec.has_index {
                SaveGroup::CloudSynced
            } else {
                SaveGroup::OrphanNoIndex
            }
        }
    }
}

fn scan_worker(tx: mpsc::SyncSender<Msg>, ctx: egui::Context) {
    let t0 = Instant::now();
    macro_rules! send {
        ($m:expr) => {{
            let _ = tx.send($m);
            ctx.request_repaint();
        }};
    }

    send!(Msg::Stage("定位 Steam 路径 / 库 / 账号…".to_string()));
    let loc = match locator::locate() {
        Ok(l) => l,
        Err(e) => {
            send!(Msg::Error(e));
            return;
        }
    };
    for w in &loc.warnings {
        send!(Msg::Warning(w.clone()));
    }
    let (installed, w) = manifest::scan(&loc.libraries);
    for x in w {
        send!(Msg::Warning(x));
    }
    send!(Msg::Env { loc: loc.clone(), installed: installed.len() });

    send!(Msg::Stage("扫描安装残留…".to_string()));
    let tx_o = tx.clone();
    let ctx_o = ctx.clone();
    let on_orphan = move |it: &OrphanDir| {
        let _ = tx_o.send(Msg::OrphanItem(it.clone()));
        ctx_o.request_repaint();
    };
    let (orep, w) = orphan::scan(&loc.libraries, &installed, Some(&on_orphan));
    for x in w {
        send!(Msg::Warning(x));
    }
    send!(Msg::OrphanTotals {
        common_count: orep.common_count,
        common_bytes: orep.common_bytes,
        verified: orep.common_autocloud_verified,
        other_count: orep.other_count,
        other_bytes: orep.other_bytes,
    });

    send!(Msg::Stage("整理游戏存档…".to_string()));
    let tx_s = tx.clone();
    let ctx_s = ctx.clone();
    let on_save = move |rec: &AppSaveRecord| {
        let _ = tx_s.send(Msg::SaveItem(rec.clone()));
        ctx_s.request_repaint();
    };
    let (srep, w) = savemap::scan(&loc.steam_path, &loc.accounts, &installed, Some(&on_save));
    for x in w {
        send!(Msg::Warning(x));
    }
    send!(Msg::SaveTotals {
        total_apps: srep.total_apps,
        orphan_apps: srep.orphan_apps,
        total_bytes: srep.userdata_total_bytes,
        orphan_bytes: srep.userdata_orphan_bytes,
        red: srep.red_alert_apps,
        empty: srep.empty_shells,
        rwoi: srep.remote_without_index,
    });

    // ---- M3：appinfo.vdf ufs 补定位 ----
    // remotecache.vdf 索引丢失的已卸载游戏，用 Steam 官方 Auto-Cloud 配置
    // 补定位存档目录，合成"仅本地痕迹"记录汇入存档列表。
    send!(Msg::Stage("解析 appinfo 存档配置…".to_string()));
    let indexed: HashSet<u32> = srep
        .records
        .iter()
        .filter(|r| r.has_index)
        .map(|r| r.appid)
        .collect();
    let ufs_recs = savemap::ufs_supplement(&loc.steam_path, &loc.accounts, &installed, &indexed);
    let ufs_new = ufs_recs.len();
    for rec in ufs_recs {
        send!(Msg::SaveItem(rec));
    }
    if ufs_new > 0 {
        send!(Msg::Warning(format!(
            "appinfo 官方配置补充定位了 {} 个游戏的存档目录",
            ufs_new
        )));
    }

    send!(Msg::Stage("扫描共性痕迹…".to_string()));
    let installed_ids: HashSet<u32> = installed.keys().copied().collect();
    let mut covered: HashSet<String> = HashSet::new();
    for rec in &srep.records {
        for d in &rec.local_save_dirs {
            covered.insert(util::normalize_path(d));
        }
    }
    let tx_t = tx.clone();
    let ctx_t = ctx.clone();
    let on_trace = move |d: &HeuristicDir| {
        let _ = tx_t.send(Msg::TraceItem(d.clone()));
        ctx_t.request_repaint();
    };
    let (hrep, w) = heuristic::scan_fast(
        &loc.steam_path,
        &installed_ids,
        &covered,
        Some(&on_trace),
        &extra_scan_dirs(),
    );
    for x in w {
        send!(Msg::Warning(x));
    }
    send!(Msg::TraceTotals {
        confirmed: hrep.confirmed.len(),
        new_dirs: hrep.confirmed_new,
        new_bytes: hrep.new_total_bytes,
        graylist: hrep.graylisted_pruned,
        stats_bin: hrep.stats_bin_appids,
        stats_orphan: hrep.stats_bin_orphan_appids,
    });

    util::save_size_cache();

    // ---- M4：启动卸载事件监听（全局一次，3 秒级 acf 快照 diff）----
    let prev: std::collections::BTreeMap<u32, String> = installed
        .iter()
        .map(|(id, a)| (*id, a.name.clone()))
        .collect();
    crate::watcher::start_to_channel(&loc, prev, tx.clone(), |appid, name| Msg::Uninstalled {
        appid,
        name,
    }, ctx.clone());

    send!(Msg::Done(t0.elapsed().as_secs_f64()));
}

// ---------- 封面下载 ----------
fn cover_cache_dir() -> PathBuf {
    let dir = util::env_path("LOCALAPPDATA")
        .unwrap_or_else(|| PathBuf::from("."))
        .join("SteamSweep")
        .join("covers");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Steam 内部组件 AppID（无商店页、无封面，商店抓取必然失败）：
/// 直接给友好名，避免显示"未知游戏"且省去徒劳的联网。
fn well_known_app_name(appid: u32) -> Option<&'static str> {
    Some(match appid {
        7 | 3 | 8 | 92 | 93 => "Steam 旧版组件",
        211 => "Source SDK",
        215 => "Source SDK Base 2006",
        218 => "Source SDK Base 2007",
        241100 => "Steam 控制器配置",
        228980 => "Steamworks 公用运行库",
        1070560 | 1391110 | 1628350 => "Steam Linux 运行时",
        1493710 => "Proton Experimental",
        961940 | 1054830 | 1113280 | 1245040 | 1420170 | 1580130 | 1887720 | 2348590 => {
            "Proton 兼容层"
        }
        _ => return None,
    })
}

/// 运行时/系统组件 AppID 判定（= 内置表全体成员）。默认从存档与残留列表排除。
fn is_runtime_app(appid: u32) -> bool {
    well_known_app_name(appid).is_some()
}

/// 孤儿目录名是否是运行时组件（common 白名单之外的兜底，如 "Proton - Experimental"）。
fn is_runtime_orphan(it: &OrphanDir) -> bool {
    if it.appid_hint.map(is_runtime_app).unwrap_or(false) {
        return true;
    }
    let lower = it.name.to_lowercase();
    lower.starts_with("proton")
        || lower.starts_with("steamlinuxruntime")
        || matches!(
            lower.as_str(),
            "steamworks shared" | "steam controller configs" | "steam client service" | "steamvr"
        )
}

fn spawn_cover_workers(
    appids: Vec<u32>,
    need_name: HashSet<u32>,
    steam_path: Option<PathBuf>,
    tx: mpsc::SyncSender<Msg>,
    ctx: egui::Context,
) {
    const THREADS: usize = 4;
    let cache = cover_cache_dir();
    let need_name = std::sync::Arc::new(need_name);
    let steam_path = std::sync::Arc::new(steam_path);
    let mut chunks: Vec<Vec<u32>> = vec![Vec::new(); THREADS];
    for (i, a) in appids.into_iter().enumerate() {
        chunks[i % THREADS].push(a);
    }
    for chunk in chunks {
        if chunk.is_empty() {
            continue;
        }
        let tx = tx.clone();
        let ctx = ctx.clone();
        let cache = cache.clone();
        let need_name = need_name.clone();
        let steam_path = steam_path.clone();
        std::thread::spawn(move || {
            for appid in chunk {
                let path = cache.join(format!("{}.jpg", appid));
                // 本地优先：Steam 自己的 librarycache → 我们的磁盘缓存 → CDN
                let bytes = steam_path
                    .as_ref()
                    .as_ref()
                    .and_then(|sp| local_cover(sp, appid))
                    .or_else(|| match std::fs::read(&path) {
                        Ok(b) if b.len() > 1000 => Some(b),
                        _ => download_cover(appid, &path),
                    });
                let msg = match bytes.and_then(|b| decode_cover(&b)) {
                    Some(img) => Msg::Cover { appid, img },
                    None => Msg::CoverFailed(appid),
                };
                let _ = tx.send(msg);
                if need_name.contains(&appid) {
                    let name = well_known_app_name(appid)
                        .map(|n| (n.to_string(), None))
                        .or_else(|| fetch_app_name(appid));
                    if let Some((display, en)) = name {
                        let _ = tx.send(Msg::Name { appid, display, en });
                    }
                }
                ctx.request_repaint();
            }
        });
    }
}

/// Steam 本地图库缓存：<steam>\appcache\librarycache\<appid>\。
/// 优先 header.jpg（横版小图），其次 library_hero.jpg，再次哈希命名图，最后竖版 600x900。
fn local_cover(steam_path: &std::path::Path, appid: u32) -> Option<Vec<u8>> {
    let dir = steam_path
        .join("appcache")
        .join("librarycache")
        .join(appid.to_string());
    for cand in ["header.jpg", "library_hero.jpg"] {
        if let Ok(b) = std::fs::read(dir.join(cand)) {
            if b.len() > 1000 {
                return Some(b);
            }
        }
    }
    // 哈希命名（40 位 hex）的 jpg 通常是 header 的实际存储形式
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().to_lowercase();
            if name.len() == 44
                && name.ends_with(".jpg")
                && name[..40].chars().all(|c| c.is_ascii_hexdigit())
            {
                if let Ok(b) = std::fs::read(entry.path()) {
                    if b.len() > 1000 {
                        return Some(b);
                    }
                }
            }
        }
    }
    if let Ok(b) = std::fs::read(dir.join("library_600x900.jpg")) {
        if b.len() > 1000 {
            return Some(b);
        }
    }
    None
}

/// 解码封面（工作线程执行，避免阻塞 GUI 帧）
fn decode_cover(bytes: &[u8]) -> Option<egui::ColorImage> {
    let img = image::load_from_memory(bytes).ok()?;
    let rgba = img.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    Some(egui::ColorImage::from_rgba_unmultiplied(size, &rgba))
}

/// 抓商店页面取游戏名：先抓简体中文版（有译名用译名），
/// 若译名含非 ASCII（说明和英文原名不同）再补抓英文页，供文件夹名反查。
/// 返回 (展示名, 英文名或 None)。
fn fetch_app_name(appid: u32) -> Option<(String, Option<String>)> {
    let zh = fetch_store_title(appid, "schinese")?;
    let needs_en = zh.chars().any(|c| !c.is_ascii());
    let en = if needs_en {
        fetch_store_title(appid, "english")
    } else {
        None
    };
    let en = en.filter(|e| e != &zh);
    Some((zh, en))
}

fn fetch_store_title(appid: u32, lang: &str) -> Option<String> {
    const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                      (KHTML, like Gecko) Chrome/126.0 Safari/537.36";
    let url = format!("https://store.steampowered.com/app/{}/?l={}", appid, lang);
    // 直连优先，失败退本地代理（商店页通常直连可通，代理只是兜底）
    let mut resp = None;
    for agent in http_agents() {
        if let Ok(r) = agent
            .get(&url)
            .set("User-Agent", UA)
            .timeout(Duration::from_secs(10))
            .call()
        {
            resp = Some(r);
            break;
        }
    }
    let resp = resp?;
    if resp.status() != 200 {
        return None;
    }
    let mut body = String::new();
    // 标题在 head 里，前 64KB 足够；整页可能上百 KB，不全读
    resp.into_reader()
        .take(65536)
        .read_to_string(&mut body)
        .ok()?;
    let start = body.find("<title>")? + "<title>".len();
    let end = body[start..].find("</title>")? + start;
    let raw = body[start..end].trim();
    let mut name = raw
        .strip_prefix("Steam 上的 ")
        .or_else(|| raw.strip_suffix(" on Steam"))
        .or_else(|| raw.strip_suffix(" 在 Steam 上"))
        .unwrap_or(raw)
        .trim()
        .to_string();
    // 促销标题：英文 "Save 67% on XXX"，中文 "在 Steam 上购买 XXX 立省 40%"
    if let Some(rest) = name.strip_prefix("Save ") {
        if let Some(idx) = rest.find("% on ") {
            name = rest[idx + 5..].trim().to_string();
        }
    }
    if let Some(rest) = name.strip_prefix("在 Steam 上购买 ") {
        name = rest.to_string();
        if let Some(idx) = name.rfind(" 立省 ") {
            name = name[..idx].trim().to_string();
        }
    }
    // 年龄验证/无效应用/首页跳转等标题不含游戏名，过滤
    if name.is_empty()
        || name.eq_ignore_ascii_case("steam")
        || name.contains("Age Check")
        || name.contains("Welcome to Steam")
    {
        None
    } else {
        Some(name)
    }
}

fn names_cache_path() -> PathBuf {
    cover_cache_dir()
        .parent()
        .map(|p| p.join("names.json"))
        .unwrap_or_else(|| PathBuf::from("names.json"))
}

fn names_en_cache_path() -> PathBuf {
    cover_cache_dir()
        .parent()
        .map(|p| p.join("names_en.json"))
        .unwrap_or_else(|| PathBuf::from("names_en.json"))
}

fn load_name_cache() -> HashMap<u32, String> {
    std::fs::read_to_string(names_cache_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn load_name_en_cache() -> HashMap<u32, String> {
    std::fs::read_to_string(names_en_cache_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_name_cache(names: &HashMap<u32, String>) {
    if let Ok(s) = serde_json::to_string(names) {
        let _ = std::fs::write(names_cache_path(), s);
    }
}

fn save_name_en_cache(names: &HashMap<u32, String>) {
    if let Ok(s) = serde_json::to_string(names) {
        let _ = std::fs::write(names_en_cache_path(), s);
    }
}

/// 由 names_en ∪ names 构建 小写名字 → AppID 反向索引（names_en 优先，其条目更可能等于文件夹名）。
fn build_names_rev(names_en: &HashMap<u32, String>, names: &HashMap<u32, String>) -> HashMap<String, u32> {
    let mut rev = HashMap::with_capacity(names_en.len() + names.len());
    for (appid, n) in names_en {
        rev.insert(n.to_lowercase(), *appid);
    }
    for (appid, n) in names {
        rev.entry(n.to_lowercase()).or_insert(*appid);
    }
    rev
}

// ---------- 本地配置（API key 等，只存本机 %LOCALAPPDATA%，不入库） ----------

#[derive(serde::Deserialize, serde::Serialize, Default)]
struct LocalConfig {
    steam_api_key: Option<String>,
    /// 本地代理（如 http://127.0.0.1:7890）；不默认启用，需在设置中显式填写
    proxy: Option<String>,
    /// 自定义痕迹扫描目录（每行一个，M3）
    extra_scan_dirs: Option<Vec<String>>,
    /// "不再提醒"的卸载联动名单（M4）
    muted_appids: Option<Vec<u32>>,
    /// 上次成功拉全量名称表的 unix 秒
    applist_fetched_at: Option<u64>,
}

fn config_path() -> PathBuf {
    cover_cache_dir()
        .parent()
        .map(|p| p.join("config.json"))
        .unwrap_or_else(|| PathBuf::from("config.json"))
}

fn load_local_config() -> LocalConfig {
    std::fs::read_to_string(config_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 设置里的自定义扫描目录（过滤不存在的路径）
fn extra_scan_dirs() -> Vec<PathBuf> {
    load_local_config()
        .extra_scan_dirs
        .unwrap_or_default()
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.exists())
        .collect()
}

/// 有 key 且全量表需要更新（条目过少或超过 7 天未拉）时返回 key。
fn applist_key_if_stale(names_en_len: usize) -> Option<String> {
    let cfg = load_local_config();
    let key = cfg.steam_api_key.filter(|k| !k.trim().is_empty())?;
    let stale = cfg
        .applist_fetched_at
        .map(|t| now_unix().saturating_sub(t) > 7 * 86400)
        .unwrap_or(true);
    if names_en_len < 10000 || stale {
        Some(key)
    } else {
        None
    }
}

/// 分页拉 IStoreService/GetAppList（include_games，每页 5 万）。
/// api.steampowered.com 被间歇性 SNI 阻断：每页重试数次，整页失败即静默放弃；
/// 全部成功后回发 NamesBulk，并把 fetched_at 写回 config（防 7 天内重复拉）。
fn applist_worker(key: String, tx: mpsc::SyncSender<Msg>, ctx: egui::Context) {
    let mut all: Vec<(u32, String)> = Vec::new();
    let mut last_appid = 0u64;
    loop {
        let url = format!(
            "https://api.steampowered.com/IStoreService/GetAppList/v1/?key={}&include_games=1&max_results=50000&last_appid={}",
            key, last_appid
        );
        let mut page: Option<serde_json::Value> = None;
        'retry: for attempt in 0..6u64 {
            // 每次尝试都走 直连 → 本地代理 双通道（SNI 阻断时代理通常可用）
            for agent in http_agents() {
                if let Ok(resp) = agent.get(&url).timeout(Duration::from_secs(20)).call() {
                    if resp.status() == 200 {
                        page = resp
                            .into_string()
                            .ok()
                            .and_then(|s| serde_json::from_str(&s).ok());
                        break 'retry;
                    }
                }
            }
            std::thread::sleep(Duration::from_secs(3 + attempt * 4));
        }
        let Some(v) = page else { return };
        let resp = &v["response"];
        let Some(apps) = resp["apps"].as_array() else { return };
        for a in apps {
            if let (Some(id), Some(name)) = (a["appid"].as_u64(), a["name"].as_str()) {
                let name = name.trim();
                if !name.is_empty() && id <= u32::MAX as u64 {
                    all.push((id as u32, name.to_string()));
                }
            }
        }
        if resp["more_results"].as_bool() == Some(true) {
            match resp["last_appid"].as_u64() {
                Some(la) if la > last_appid => last_appid = la,
                _ => break,
            }
        } else {
            break;
        }
    }
    if all.len() > 10000 {
        // 成功后记录时间戳（保留原 key 字段）
        let mut cfg = load_local_config();
        cfg.applist_fetched_at = Some(now_unix());
        if let Ok(s) = serde_json::to_string(&cfg) {
            let _ = std::fs::write(config_path(), s);
        }
        let _ = tx.send(Msg::NamesBulk(all));
        ctx.request_repaint();
    }
}

/// 直连 Agent + 可选本地代理 Agent（api.steampowered.com 被 SNI 阻断时兜底）。
/// 代理地址来自设置（config.json 的 "proxy" 字段），不默认启用。
/// DoH 解决不了 SNI 阻断（封锁在 TLS 握手而非 DNS），只能靠代理或重试。
fn http_agents() -> &'static [ureq::Agent] {
    static AGENTS: std::sync::OnceLock<Vec<ureq::Agent>> = std::sync::OnceLock::new();
    AGENTS.get_or_init(|| {
        // ureq 不会自动启用 native-tls，必须显式构建 Agent（见 ureq 文档）
        let connector = native_tls::TlsConnector::new().expect("init native-tls");
        let connector = std::sync::Arc::new(connector);
        let mut v = vec![ureq::AgentBuilder::new()
            .tls_connector(connector.clone())
            .build()];
        let cfg = load_local_config();
        // 不默认启用代理：只有用户在设置里显式填写了代理地址才走代理
        let proxy = cfg.proxy.filter(|p| !p.trim().is_empty());
        if let Some(p) = proxy {
            if let Ok(px) = ureq::Proxy::new(&p) {
                v.push(
                    ureq::AgentBuilder::new()
                        .tls_connector(connector)
                        .proxy(px)
                        .build(),
                );
            }
        }
        v
    })
    .as_slice()
}

fn download_cover(appid: u32, cache_path: &std::path::Path) -> Option<Vec<u8>> {
    // 两种横版图源 × （直连 / 代理），header 优先；404 或网络错误都继续试下一个
    let urls = [
        format!("https://cdn.cloudflare.steamstatic.com/steam/apps/{}/header.jpg", appid),
        format!("https://cdn.cloudflare.steamstatic.com/steam/apps/{}/capsule_616x353.jpg", appid),
    ];
    for url in &urls {
        for agent in http_agents() {
            let Ok(resp) = agent.get(url).timeout(Duration::from_secs(15)).call() else {
                continue;
            };
            if resp.status() != 200 {
                break; // 404 说明这个图源没有，换下一个 URL
            }
            let mut buf = Vec::new();
            if resp.into_reader().take(3_000_000).read_to_end(&mut buf).is_err() {
                continue;
            }
            if buf.len() < 1000 {
                break;
            }
            let _ = std::fs::write(cache_path, &buf);
            return Some(buf);
        }
    }
    None
}

impl eframe::App for SteamSweepApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_messages(ctx);
        if self.scanning || self.cleaning || self.covers.values().any(|c| matches!(c, CoverState::Loading)) {
            ctx.request_repaint_after(Duration::from_millis(150));
        }

        // ---------- 顶栏：状态条 ----------
        egui::TopBottomPanel::top("top")
            .frame(egui::Frame::default().fill(BG_PANEL).inner_margin(egui::Margin::symmetric(14, 10)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if self.scanning {
                        // 扫描中就只显示"扫描中"
                        ui.spinner();
                        ui.label(RichText::new("扫描中…").size(15.0).color(ACCENT));
                    } else {
                        ui.label(RichText::new(&self.stage).size(13.0).color(GRAY));
                        if let Some(secs) = self.elapsed {
                            ui.label(RichText::new(format!("总耗时 {:.1}s", secs)).size(13.0).color(GRAY));
                        }
                        ui.separator();
                        if ui.button("↻ 重新扫描").clicked() {
                            self.start_scan(ctx);
                        }
                    }
                });
            });

        // ---------- 左侧导航 ----------
        egui::SidePanel::left("nav")
            .resizable(false)
            .exact_width(NAV_W)
            .frame(egui::Frame::default().fill(BG_PANEL).inner_margin(egui::Margin::symmetric(12, 16)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if let Some(t) = &self.logo_tex {
                        ui.add(
                            egui::Image::new(egui::load::SizedTexture::new(t.id(), Vec2::splat(38.0)))
                                .corner_radius(9.0),
                        );
                        ui.add_space(4.0);
                    }
                    ui.vertical(|ui| {
                        ui.add_space(2.0);
                        ui.label(RichText::new("SteamSweep").size(17.0).strong().color(Color32::WHITE));
                        ui.label(RichText::new("Steam 残留清理").size(11.0).color(GRAY));
                    });
                });
                ui.add_space(26.0);

                if nav_button(ui, "安装残留", self.tab == Tab::Installs) {
                    self.tab = Tab::Installs;
                }
                ui.add_space(4.0);
                if nav_button(ui, "存档清理", self.tab == Tab::Saves) {
                    self.tab = Tab::Saves;
                }
                ui.add_space(4.0);
                if nav_button(ui, "更多痕迹", self.tab == Tab::Traces) {
                    self.tab = Tab::Traces;
                }

                ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                    if nav_button(ui, "设  置", false) {
                        // 打开时回填当前配置
                        let cfg = load_local_config();
                        self.proxy_input = cfg.proxy.unwrap_or_default();
                        self.apikey_input = cfg.steam_api_key.unwrap_or_default();
                        self.dirs_input = cfg.extra_scan_dirs.unwrap_or_default().join("\n");
                        self.settings_open = true;
                    }
                });
            });

        // ---------- 底栏：勾选汇总 ----------
        egui::TopBottomPanel::bottom("bottom")
            .frame(egui::Frame::default().fill(BG_PANEL).inner_margin(egui::Margin::symmetric(14, 9)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let (n, bytes) = self.selected_totals();
                    if n > 0 {
                        ui.label(
                            RichText::new(format!("已勾选 {} 项 · 合计 {}", n, util::human_size(bytes)))
                                .strong()
                                .color(YELLOW),
                        );
                        ui.separator();
                        if ui.button("清空勾选").clicked() {
                            self.sel_installs.clear();
                            self.sel_saves.clear();
                        }
                        ui.separator();
                        let can_clean = !self.scanning && !self.cleaning;
                        if ui
                            .add_enabled(
                                can_clean,
                                egui::Button::new(RichText::new("开始清理").strong().color(Color32::WHITE))
                                    .fill(Color32::from_rgb(76, 140, 64)),
                            )
                            .clicked()
                        {
                            self.confirm_open = true;
                        }
                        ui.separator();
                    }
                    if !self.warnings.is_empty() {
                        ui.label(RichText::new(format!("{} 条提示", self.warnings.len())).color(YELLOW))
                            .on_hover_text(self.warnings.join("\n"));
                    }
                });
            });

        // ---------- 中央 ----------
        let central_frame = egui::Frame::default()
            .fill(BG_DEEP)
            .inner_margin(egui::Margin::symmetric(14, 10));
        egui::CentralPanel::default().frame(central_frame).show(ctx, |ui| {
            // 卸载联动横幅（M4）
            if let Some(b) = self.banner.clone() {
                let frame = egui::Frame::default()
                    .fill(Color32::from_rgb(58, 50, 28))
                    .corner_radius(8.0)
                    .stroke(Stroke::new(1.0_f32, Color32::from_rgb(120, 100, 50)))
                    .inner_margin(egui::Margin::same(10));
                frame.show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(format!("《{}》已卸载", b.name))
                                .strong()
                                .size(16.0)
                                .color(YELLOW),
                        );
                        ui.separator();
                        let mut parts = Vec::new();
                        if b.save_count > 0 {
                            parts.push(format!("存档 {} 个 · {}", b.save_count, util::human_size(b.save_bytes)));
                        }
                        if b.orphan_count > 0 {
                            parts.push(format!("安装残留 {} 项 · {}", b.orphan_count, util::human_size(b.orphan_bytes)));
                        }
                        ui.label(parts.join("，"));
                        if b.has_red {
                            ui.label(RichText::new("⚠ 存档疑似从未上传云端").color(RED));
                        }
                    });
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        if ui.button("查看存档").clicked() {
                            self.tab = Tab::Saves;
                            self.saves_filter = b.appid.to_string();
                        }
                        if ui.button("选中相关残留").clicked() {
                            for r in &self.saves {
                                if r.appid == b.appid {
                                    self.sel_saves.insert(save_key(r));
                                }
                            }
                            for it in &self.orphans {
                                if it.appid_hint == Some(b.appid) || it.name.eq_ignore_ascii_case(&b.name) {
                                    self.sel_installs.insert(it.path.to_string_lossy().to_string());
                                }
                            }
                        }
                        if ui.button("不再提醒此游戏").clicked() {
                            let mut cfg = load_local_config();
                            let mut m = cfg.muted_appids.unwrap_or_default();
                            if !m.contains(&b.appid) {
                                m.push(b.appid);
                            }
                            cfg.muted_appids = Some(m);
                            if let Ok(s) = serde_json::to_string(&cfg) {
                                let _ = std::fs::write(config_path(), s);
                            }
                            self.banner = None;
                        }
                        if ui.button("✕").clicked() {
                            self.banner = None;
                        }
                    });
                });
                ui.add_space(6.0);
            }
            // 汇总卡片
            ui.horizontal(|ui| {
                if let Some((cc_, cb_, _v, oc, ob)) = self.orphan_totals {
                    stat_card(
                        ui,
                        "安装残留",
                        &format!("{} 项 · {}", cc_ + oc, util::human_size(cb_ + ob)),
                        "",
                        GREEN,
                    );
                } else {
                    stat_card(ui, "安装残留", "扫描中…", "", GRAY);
                }
                ui.add_space(8.0);
                if self.scanning && self.saves.is_empty() {
                    stat_card(ui, "存档残留", "扫描中…", "", GRAY);
                } else {
                    let vis = |r: &&AppSaveRecord| self.show_runtime || !is_runtime_app(r.appid);
                    let shown = self.saves.iter().filter(vis).count();
                    let shown_bytes: u64 = self.saves.iter().filter(vis).map(|r| r.userdata_bytes).sum();
                    let red = self.save_totals.map(|t| t.4).unwrap_or(0);
                    stat_card(
                        ui,
                        "存档残留",
                        &format!("{} 个游戏 · {}", shown, util::human_size(shown_bytes)),
                        &format!("{} 个疑似从未上传云端", red),
                        if red > 0 { RED } else { GREEN },
                    );
                }
            });
            ui.add_space(6.0);

            // 工具行：按当前页签显示筛选 / 运行时开关
            if self.tab != Tab::Traces {
                ui.horizontal(|ui| {
                    if self.tab == Tab::Saves {
                        ui.label(RichText::new("筛选").color(GRAY).size(13.0));
                        ui.add(
                            egui::TextEdit::singleline(&mut self.saves_filter)
                                .desired_width(200.0)
                                .hint_text("游戏名或 AppID"),
                        );
                        if !self.saves_filter.is_empty() && ui.button("清除").clicked() {
                            self.saves_filter.clear();
                        }
                        ui.separator();
                    }
                    // 默认排除 Steam 运行时组件（Proton / 运行库等，非游戏）
                    ui.checkbox(&mut self.show_runtime, "显示运行时组件");
                });
                ui.add_space(6.0);
            }

            match self.tab {
                Tab::Installs => installs_view(ui, self),
                Tab::Saves => saves_view(ui, self),
                Tab::Traces => {
                    let app_ref: &SteamSweepApp = self;
                    traces_table(ui, app_ref)
                }
            }
        });

        // ---------- 设置窗口 ----------
        let mut settings_open = self.settings_open;
        egui::Window::new("设置")
            .open(&mut settings_open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.set_min_width(420.0);
                ui.label(RichText::new("网络").strong());
                ui.horizontal(|ui| {
                    ui.label("代理地址：");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.proxy_input)
                            .desired_width(240.0)
                            .hint_text("留空 = 不使用代理（推荐）"),
                    );
                });
                ui.label(
                    RichText::new("仅当 Steam 接口被阻断时才需要，例如 http://127.0.0.1:7890")
                        .size(12.0)
                        .color(GRAY),
                );
                ui.add_space(8.0);
                ui.label(RichText::new("Steam API 密钥（用于补全游戏名，可选）").strong());
                ui.horizontal(|ui| {
                    ui.label("API Key：");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.apikey_input)
                            .desired_width(240.0)
                            .password(true),
                    );
                });
                ui.add_space(8.0);
                ui.label(RichText::new("额外扫描目录（每行一个，可选）").strong());
                ui.label(
                    RichText::new("默认已覆盖 AppData / Documents / Saved Games；其他盘的存档目录可加在这里")
                        .size(12.0)
                        .color(GRAY),
                );
                ui.add(
                    egui::TextEdit::multiline(&mut self.dirs_input)
                        .desired_width(380.0)
                        .desired_rows(3),
                );
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button("保存").clicked() {
                        let mut cfg = load_local_config();
                        let p = self.proxy_input.trim();
                        cfg.proxy = if p.is_empty() { None } else { Some(p.to_string()) };
                        let k = self.apikey_input.trim();
                        if !k.is_empty() {
                            cfg.steam_api_key = Some(k.to_string());
                        }
                        let dirs: Vec<String> = self
                            .dirs_input
                            .lines()
                            .map(|l| l.trim().to_string())
                            .filter(|l| !l.is_empty())
                            .collect();
                        cfg.extra_scan_dirs = if dirs.is_empty() { None } else { Some(dirs) };
                        if let Ok(s) = serde_json::to_string(&cfg) {
                            let _ = std::fs::write(config_path(), s);
                        }
                        self.settings_open = false;
                    }
                    ui.label(RichText::new("保存后重启软件生效").size(12.0).color(YELLOW));
                });
            });
        self.settings_open = settings_open;

        // ---------- 清理确认窗口（M2） ----------
        if self.confirm_open {
            let (n, bytes) = self.selected_totals();
            let red = self.selected_red_saves();
            let installed = self.selected_installed_saves();
            let mut open = self.confirm_open;
            egui::Window::new("确认清理")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.set_min_width(460.0);
                    ui.label(
                        RichText::new(format!("即将清理 {} 项，预计释放 {}", n, util::human_size(bytes)))
                            .size(17.0)
                            .strong(),
                    );
                    ui.add_space(6.0);
                    ui.label("所有删除都进系统回收站，可以从回收站恢复。");
                    if red > 0 {
                        ui.label(
                            RichText::new(format!(
                                "⚠ 其中 {} 个存档疑似从未上传云端，删除后只能靠回收站恢复",
                                red
                            ))
                            .color(RED),
                        );
                    }
                    if installed > 0 {
                        ui.label(
                            RichText::new(format!(
                                "其中 {} 个存档属于未卸载的游戏，删除后游戏会从云端重新同步",
                                installed
                            ))
                            .color(YELLOW),
                        );
                    }
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui
                            .button(RichText::new("开始清理").strong().color(Color32::WHITE))
                            .clicked()
                        {
                            self.start_clean(ctx);
                        }
                        if ui.button("取消").clicked() {
                            self.confirm_open = false;
                        }
                    });
                });
            self.confirm_open = open && self.confirm_open;
        }

        // ---------- 清理进行中 ----------
        if self.cleaning {
            egui::Window::new("正在清理")
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
                .interactable(false)
                .show(ctx, |ui| {
                    ui.set_min_width(380.0);
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(&self.clean_progress);
                    });
                });
        }

        // ---------- 清理结果 ----------
        if self.result_open {
            if let Some(sum) = &self.clean_result {
                let mut open = self.result_open;
                egui::Window::new("清理完成")
                    .open(&mut open)
                    .collapsible(false)
                    .resizable(false)
                    .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        ui.set_min_width(480.0);
                        ui.label(
                            RichText::new(format!(
                                "已清理 {} 项 · 释放 {}",
                                sum.recycled,
                                util::human_size(sum.freed)
                            ))
                            .size(17.0)
                            .strong()
                            .color(GREEN),
                        );
                        ui.add_space(4.0);
                        if let Some(log) = &sum.log_path {
                            ui.label(RichText::new(format!("操作日志：{}", log.display())).size(12.0).color(GRAY));
                        }
                        if !sum.failed.is_empty() {
                            ui.add_space(4.0);
                            ui.label(RichText::new(format!("{} 项失败：", sum.failed.len())).color(RED));
                            for f in sum.failed.iter().take(8) {
                                ui.label(RichText::new(f).size(12.0).color(RED));
                            }
                        }
                        ui.add_space(8.0);
                        if ui.button("完成").clicked() {
                            self.result_open = false;
                        }
                    });
                self.result_open = open && self.result_open;
            }
        }
    }
}

fn stat_card(ui: &mut egui::Ui, title: &str, main: &str, sub: &str, color: Color32) {
    egui::Frame::default()
        .fill(BG_CARD)
        .corner_radius(8.0)
        .stroke(Stroke::new(1.0_f32, Color32::from_white_alpha(12)))
        .inner_margin(egui::Margin::same(12))
        .show(ui, |ui| {
            ui.set_min_width(360.0);
            ui.label(RichText::new(title).color(GRAY).size(12.0));
            ui.label(RichText::new(main).size(20.0).strong().color(color));
            if !sub.is_empty() {
                ui.label(RichText::new(sub).size(12.0).color(GRAY));
            }
        });
}

/// 封面图：真封面 → 占位插画 → 字母色块，三级回退
fn cover_widget(ui: &mut egui::Ui, tex: Option<&TextureHandle>, placeholder: Option<&TextureHandle>, seed: &str) {
    let size = Vec2::new(COVER_W, COVER_H);
    if let Some(t) = tex.or(placeholder) {
        ui.add(
            egui::Image::new(egui::load::SizedTexture::new(t.id(), size)).corner_radius(4.0),
        );
        return;
    }
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let mut h: u32 = 5381;
    for b in seed.bytes() {
        h = h.wrapping_mul(33).wrapping_add(b as u32);
    }
    let hue = (h % 360) as f32 / 360.0;
    let (r, g, b) = hsv_to_rgb(hue, 0.45, 0.35);
    ui.painter().rect_filled(rect, 4.0, Color32::from_rgb(r, g, b));
    let letter: String = seed.chars().next().map(|c| c.to_uppercase().collect()).unwrap_or_else(|| "🎮".into());
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        letter,
        FontId::proportional(22.0),
        Color32::from_white_alpha(180),
    );
}

fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (u8, u8, u8) {
    let i = (h * 6.0).floor();
    let f = h * 6.0 - i;
    let p = v * (1.0 - s);
    let q = v * (1.0 - f * s);
    let t = v * (1.0 - (1.0 - f) * s);
    let (r, g, b) = match (i as i32) % 6 {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    ((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8)
}

struct RowArgs<'a> {
    checked: bool,
    tex: Option<&'a TextureHandle>,
    /// 无封面时的占位插画（M5）
    placeholder: Option<TextureHandle>,
    seed: String,
    title: String,
    title_color: Color32,
    subtitle: String,
    /// 副标题前的小头像（存档行的账号头像）
    avatar: Option<TextureHandle>,
    right: String,
    badge: Option<(&'a str, Color32)>,
    hover: String,
    striped: bool,
}

/// 清理工具风格的统一行：复选框 + 封面 + 大字标题 + 副信息 + 右侧大小。
/// 返回勾选状态是否被改变。
fn cleaner_row(ui: &mut egui::Ui, mut args: RowArgs) -> bool {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, ROW_H), Sense::click());
    let hovered = response.hovered();
    let bg = if hovered {
        Color32::from_white_alpha(14)
    } else if args.striped {
        Color32::from_white_alpha(5)
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, 6.0, bg);

    let inner = rect.shrink2(Vec2::new(10.0, 5.0));
    let mut row_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(inner)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );

    let changed = row_ui.checkbox(&mut args.checked, "").changed();

    let tex = args.tex.cloned();
    cover_widget(&mut row_ui, tex.as_ref(), args.placeholder.as_ref(), &args.seed);
    row_ui.add_space(10.0);

    let text_width = (inner.width() - COVER_W - 200.0).max(120.0);
    row_ui.allocate_ui_with_layout(
        Vec2::new(text_width, inner.height()),
        egui::Layout::top_down(egui::Align::LEFT),
        |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(&args.title).size(16.0).strong().color(args.title_color));
                if let Some((badge, color)) = args.badge {
                    ui.label(RichText::new(badge).size(11.0).color(color));
                }
            });
            ui.horizontal(|ui| {
                if let Some(av) = &args.avatar {
                    ui.add(
                        egui::Image::new(egui::load::SizedTexture::new(av.id(), Vec2::splat(16.0)))
                            .corner_radius(8.0),
                    );
                }
                ui.label(RichText::new(&args.subtitle).size(12.0).color(GRAY));
            });
        },
    );

    row_ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        ui.label(RichText::new(&args.right).size(16.0).strong());
    });

    let clicked = response.clicked();
    if !args.hover.is_empty() {
        response.on_hover_text(&args.hover);
    }
    if clicked {
        args.checked = !args.checked;
        true
    } else {
        changed
    }
}

// ---------- 安装残留 ----------
fn installs_view(ui: &mut egui::Ui, app: &mut SteamSweepApp) {
    let show_rt = app.show_runtime;
    let visible = app.orphans.iter().filter(|it| show_rt || !is_runtime_orphan(it)).count();
    if visible == 0 {
        if app.scanning {
            scanning_hint(ui);
        } else {
            empty_state(ui, app.empty_tex.as_ref(), "很干净，没有发现安装残留");
        }
        return;
    }
    ui.horizontal(|ui| {
        if ui.button("全选").clicked() {
            for it in &app.orphans {
                if show_rt || !is_runtime_orphan(it) {
                    app.sel_installs.insert(it.path.to_string_lossy().to_string());
                }
            }
        }
        if ui.button("全不选").clicked() {
            app.sel_installs.clear();
        }
        ui.label(RichText::new("按占用空间从大到小排列").color(GRAY).size(12.0));
    });
    ui.add_space(4.0);
    egui::ScrollArea::vertical().show(ui, |ui| {
        let items = std::mem::take(&mut app.orphans);
        let mut row_i = 0usize;
        for it in items.iter() {
            if !show_rt && is_runtime_orphan(it) {
                continue;
            }
            let i = row_i;
            row_i += 1;
            let key = it.path.to_string_lossy().to_string();
            let checked = app.sel_installs.contains(&key);
            let cat_label = match it.category {
                Category::Common => "游戏安装目录",
                Category::Downloading => "未完成的下载",
                Category::Workshop => "创意工坊内容",
                Category::Shadercache => "着色器缓存",
            };
            let subtitle = format!("{} · 最后使用 {}", cat_label, util::fmt_epoch(it.last_modified_epoch));
            let cover_appid = app.orphan_appid(it);
            let tex = app.cover_for(cover_appid);
            let placeholder = app.placeholder_tex.clone();
            // 标题优先用商店补全的游戏名，其次文件夹名
            let title = cover_appid
                .and_then(|a| app.names.get(&a))
                .cloned()
                .unwrap_or_else(|| it.name.clone());
            let changed = cleaner_row(
                ui,
                RowArgs {
                    checked,
                    tex,
                    placeholder,
                    seed: it.name.clone(),
                    title,
                    title_color: Color32::WHITE,
                    subtitle,
                    avatar: None,
                    right: util::human_size(it.size_bytes),
                    badge: None,
                    hover: it.path.display().to_string(),
                    striped: i % 2 == 0,
                },
            );
            if changed {
                if app.sel_installs.contains(&key) {
                    app.sel_installs.remove(&key);
                } else {
                    app.sel_installs.insert(key);
                }
            }
        }
        app.orphans = items;
    });
}

// ---------- 存档清理 ----------
fn saves_view(ui: &mut egui::Ui, app: &mut SteamSweepApp) {
    let needle = app.saves_filter.trim().to_lowercase();
    let show_rt = app.show_runtime;
    let matches = |rec: &AppSaveRecord| {
        (show_rt || !is_runtime_app(rec.appid))
            && (needle.is_empty()
                || rec.appid.to_string().contains(&needle)
                || rec.name.as_deref().map(|n| n.to_lowercase().contains(&needle)).unwrap_or(false))
    };

    // 取出 saves 避免与 &mut app 的借用冲突（渲染完放回）
    let saves = std::mem::take(&mut app.saves);
    if !saves.iter().any(|r| matches(r)) {
        if app.scanning {
            scanning_hint(ui);
        } else if needle.is_empty() {
            empty_state(ui, app.empty_tex.as_ref(), "很干净，没有发现存档残留");
        } else {
            empty_state(ui, app.empty_tex.as_ref(), "没有匹配的存档");
        }
        app.saves = saves;
        return;
    }
    egui::ScrollArea::vertical().show(ui, |ui| {
        let groups = [
            (SaveGroup::CloudSynced, "可安全删除", "云端有副本", GREEN),
            (SaveGroup::OrphanNoIndex, "仅本地痕迹", "对应游戏已找不到", BLUE),
            (SaveGroup::NeverSynced, "有丢失风险", "从未上传云端，删除前请先备份", RED),
            (SaveGroup::StillInstalled, "未卸载", "存档正在使用，一般无需处理", GRAY),
        ];
        let mut row_idx = 0usize;
        for (g, title, desc, color) in groups {
            let rows: Vec<&AppSaveRecord> = saves.iter().filter(|r| group_of(r) == g && matches(r)).collect();
            if rows.is_empty() {
                continue;
            }
            let total: u64 = rows.iter().map(|r| r.userdata_bytes).sum();
            let collapsed = app.collapsed_groups.contains(&g);
            let all_checked = !rows.is_empty() && rows.iter().all(|r| app.sel_saves.contains(&save_key(r)));

            let (collapse_clicked, check_to) =
                group_section_header(ui, collapsed, all_checked, title, desc, rows.len(), total, color);

            if collapse_clicked {
                if collapsed {
                    app.collapsed_groups.remove(&g);
                } else {
                    app.collapsed_groups.insert(g);
                }
            }
            if let Some(v) = check_to {
                for r in &rows {
                    if v {
                        app.sel_saves.insert(save_key(r));
                    } else {
                        app.sel_saves.remove(&save_key(r));
                    }
                }
            }
            if !collapsed {
                for rec in rows {
                    save_row(ui, app, rec, row_idx % 2 == 0);
                    row_idx += 1;
                }
            }
            ui.add_space(10.0);
        }
    });
    app.saves = saves;
}

/// 分组头：折叠箭头 + 全选复选框 + 标题 + 统计 + 简述。
/// 返回 (是否点了折叠, 复选框新状态)。
fn group_section_header(
    ui: &mut egui::Ui,
    collapsed: bool,
    all_checked: bool,
    title: &str,
    desc: &str,
    count: usize,
    bytes: u64,
    color: Color32,
) -> (bool, Option<bool>) {
    let mut collapse_clicked = false;
    let mut check_to: Option<bool> = None;
    egui::Frame::default()
        .fill(BG_CARD)
        .inner_margin(egui::Margin::symmetric(10, 8))
        .corner_radius(8.0)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let arrow = if collapsed { "▶" } else { "▼" };
                if ui
                    .add(egui::Label::new(RichText::new(arrow).size(13.0).color(GRAY)).sense(Sense::click()))
                    .clicked()
                {
                    collapse_clicked = true;
                }
                let mut checked = all_checked;
                if ui.checkbox(&mut checked, "").changed() {
                    check_to = Some(checked);
                }
                if ui
                    .add(
                        egui::Label::new(RichText::new(title).size(16.0).strong().color(color))
                            .sense(Sense::click()),
                    )
                    .clicked()
                {
                    collapse_clicked = true;
                }
                ui.label(
                    RichText::new(format!("{} 个 · {}", count, util::human_size(bytes)))
                        .size(14.0)
                        .color(Color32::WHITE),
                );
                ui.label(RichText::new(desc).size(12.0).color(GRAY));
            });
        });
    ui.add_space(2.0);
    (collapse_clicked, check_to)
}

fn save_row(ui: &mut egui::Ui, app: &mut SteamSweepApp, rec: &AppSaveRecord, striped: bool) {
    let key = save_key(rec);
    let checked = app.sel_saves.contains(&key);
    let (badge, title_color): (Option<(&str, Color32)>, Color32) = match rec.sync {
        SyncVerdict::NeverUploadedLocalPresent => (Some(("⚠ 从未上传云端", RED)), Color32::WHITE),
        SyncVerdict::PossiblyUnsynced | SyncVerdict::Mixed => (Some(("有进度可能未同步", YELLOW)), Color32::WHITE),
        _ if rec.installed => (None, GRAY),
        _ => (None, Color32::WHITE),
    };
    let title = rec
        .name
        .clone()
        .or_else(|| app.names.get(&rec.appid).cloned())
        .or_else(|| app.names_en.get(&rec.appid).cloned())
        .or_else(|| crate::savedb::name(rec.appid).map(str::to_string))
        .or_else(|| well_known_app_name(rec.appid).map(str::to_string))
        .unwrap_or_else(|| format!("未知游戏（AppID {}）", rec.appid));
    let acct = app.account_label(rec.account_id);
    let subtitle = if rec.cloud_files > 0 {
        format!(
            "本地 {} · 云端 {} 个文件 · {}",
            util::human_size(rec.userdata_bytes),
            rec.cloud_files,
            acct
        )
    } else {
        format!("本地 {} · {}", util::human_size(rec.userdata_bytes), acct)
    };
    let hover = if rec.local_save_dirs.is_empty() {
        "（本地已无存档文件，仅 Steam 记录）".to_string()
    } else {
        rec.local_save_dirs
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join("\n")
    };
    let tex = app.cover_for(Some(rec.appid)).cloned();
    let placeholder = app.placeholder_tex.clone();
    let avatar = app.avatar_for(ui.ctx(), rec.account_id);
    let changed = cleaner_row(
        ui,
        RowArgs {
            checked,
            tex: tex.as_ref(),
            placeholder,
            seed: format!("{}", rec.appid),
            title,
            title_color,
            subtitle,
            avatar,
            right: util::human_size(rec.userdata_bytes),
            badge,
            hover,
            striped,
        },
    );
    if changed {
        if app.sel_saves.contains(&key) {
            app.sel_saves.remove(&key);
        } else {
            app.sel_saves.insert(key);
        }
    }
}

// ---------- 更多痕迹 ----------
fn traces_table(ui: &mut egui::Ui, app: &SteamSweepApp) {
    let traces = &app.traces;
    if traces.is_empty() {
        if app.scanning {
            scanning_hint(ui);
        } else {
            empty_state(ui, app.empty_tex.as_ref(), "没有发现其他存档痕迹");
        }
        return;
    }
    ui.label(
        RichText::new("这些是 Steam 官方同步标记（steam_autocloud.vdf）出现过的目录 —— 都是真实的游戏存档目录")
            .color(GRAY)
            .size(12.0),
    );
    ui.add_space(4.0);
    let text_height = egui::TextStyle::Body.resolve(ui.style()).size;
    TableBuilder::new(ui)
        .striped(true)
        .resizable(true)
        .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
        .column(Column::auto().at_least(110.0))
        .column(Column::remainder().at_least(420.0))
        .column(Column::auto().at_least(90.0))
        .column(Column::auto().at_least(90.0))
        .header(22.0, |mut header| {
            header.col(|ui| {
                ui.strong("状态");
            });
            header.col(|ui| {
                ui.strong("目录");
            });
            header.col(|ui| {
                ui.strong("账号");
            });
            header.col(|ui| {
                ui.strong("大小");
            });
        })
        .body(|mut body| {
            for d in traces {
                body.row(text_height + 6.0, |mut row| {
                    row.col(|ui| {
                        if d.covered_by_cloud_index {
                            ui.label(RichText::new("已在存档表中").color(GRAY));
                        } else {
                            ui.label(RichText::new("✔ 存档目录").color(GREEN))
                                .on_hover_text(d.reasons.join("\n"));
                        }
                    });
                    row.col(|ui| {
                        // SaveDB 后缀归属：认出是哪个游戏的存档目录就在路径前标注
                        let norm = util::normalize_path(&d.path);
                        let owner = crate::savedb::match_path(&norm).and_then(|a| {
                            app.names
                                .get(&a)
                                .cloned()
                                .or_else(|| app.names_en.get(&a).cloned())
                                .or_else(|| crate::savedb::name(a).map(str::to_string))
                        });
                        if let Some(name) = owner {
                            ui.label(
                                RichText::new(format!("{} — {}", name, d.path.display()))
                                    .color(BLUE),
                            )
                            .on_hover_text("由存档路径数据库（SaveDB）归属");
                        } else {
                            ui.label(d.path.display().to_string());
                        }
                    });
                    row.col(|ui| {
                        let acc = d
                            .autocloud_account
                            .map(|a| app.account_label(a))
                            .unwrap_or_else(|| "-".to_string());
                        ui.label(RichText::new(acc).color(GRAY));
                    });
                    row.col(|ui| {
                        ui.label(util::human_size(d.size_bytes));
                    });
                });
            }
        });
}

/// 左侧导航大按钮：选中态 = 高亮底 + 左侧强调条
fn nav_button(ui: &mut egui::Ui, label: &str, selected: bool) -> bool {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 42.0), Sense::click());
    let bg = if selected {
        NAV_SELECTED
    } else if response.hovered() {
        Color32::from_white_alpha(10)
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, 8.0, bg);
    if selected {
        let bar = Rect::from_min_size(rect.min + Vec2::new(2.0, 8.0), Vec2::new(4.0, rect.height() - 16.0));
        ui.painter().rect_filled(bar, 2.0, ACCENT);
    }
    let color = if selected { Color32::WHITE } else { GRAY };
    ui.painter().text(
        rect.left_center() + Vec2::new(16.0, 0.0),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(15.0),
        color,
    );
    response.clicked()
}

/// 空状态：插画 + 一句话
fn empty_state(ui: &mut egui::Ui, tex: Option<&TextureHandle>, text: &str) {
    ui.vertical_centered(|ui| {
        ui.add_space(64.0);
        if let Some(t) = tex {
            let sz = t.size_vec2();
            let w = 240.0_f32;
            let h = if sz.x > 0.0 { w * sz.y / sz.x } else { w };
            ui.add(egui::Image::new(egui::load::SizedTexture::new(t.id(), Vec2::new(w, h))));
            ui.add_space(14.0);
        }
        ui.label(RichText::new(text).size(16.0).color(GRAY));
    });
}

/// 列表扫描中的轻提示（顶部状态条已有 spinner，这里只留一句话）
fn scanning_hint(ui: &mut egui::Ui) {
    ui.vertical_centered(|ui| {
        ui.add_space(80.0);
        ui.label(RichText::new("扫描中…").size(15.0).color(GRAY));
    });
}

/// 解码内嵌图片为纹理
fn load_tex(ctx: &egui::Context, name: &str, bytes: &[u8]) -> Option<TextureHandle> {
    decode_cover(bytes).map(|img| ctx.load_texture(name, img, egui::TextureOptions::LINEAR))
}

/// 解码 logo 为窗口图标
fn load_icon() -> Option<egui::IconData> {
    let img = image::load_from_memory(LOGO_PNG).ok()?.to_rgba8();
    let (width, height) = (img.width(), img.height());
    Some(egui::IconData { rgba: img.into_raw(), width, height })
}

/// egui 默认字体不含 CJK，加载系统中文字体（微软雅黑优先）。
fn install_cjk_fonts(ctx: &egui::Context) {
    let candidates = [
        r"C:\Windows\Fonts\msyh.ttc",
        r"C:\Windows\Fonts\msyh.ttf",
        r"C:\Windows\Fonts\Deng.ttf",
        r"C:\Windows\Fonts\simhei.ttf",
        r"C:\Windows\Fonts\simsun.ttc",
    ];
    for path in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            let mut fonts = egui::FontDefinitions::default();
            fonts
                .font_data
                .insert("cjk".to_string(), std::sync::Arc::new(egui::FontData::from_owned(bytes)));
            fonts
                .families
                .entry(egui::FontFamily::Proportional)
                .or_default()
                .insert(0, "cjk".to_string());
            fonts
                .families
                .entry(egui::FontFamily::Monospace)
                .or_default()
                .push("cjk".to_string());
            ctx.set_fonts(fonts);
            return;
        }
    }
}
