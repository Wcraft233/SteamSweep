//! watcher：卸载事件联动提醒（M4）。
//! 实现方式：每 3 秒对全部库做轻量 acf 快照 diff（manifest::scan 本身只有毫秒级），
//! 发现"之前安装、现在消失"的 AppID 即判定为卸载事件，通知 GUI 并弹系统 Toast。
//! 与清理主流程解耦：只发消息，不碰任何文件。

use crate::locator::Location;
use crate::manifest;
use eframe::egui;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

static STARTED: AtomicBool = AtomicBool::new(false);

/// 启动监听线程（全局只启动一次）。prev 为当前已安装快照。
pub fn start<F>(loc: &Location, prev: BTreeMap<u32, String>, mut sink: F)
where
    F: FnMut(u32, String) + Send + 'static,
{
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let libraries = loc.libraries.clone();
    std::thread::spawn(move || {
        let mut prev = prev;
        loop {
            std::thread::sleep(Duration::from_secs(3));
            let (now, _) = manifest::scan(&libraries);
            let now_map: BTreeMap<u32, String> =
                now.iter().map(|(id, a)| (*id, a.name.clone())).collect();
            for (appid, name) in &prev {
                if !now_map.contains_key(appid) {
                    sink(*appid, name.clone());
                }
            }
            prev = now_map;
        }
    });
}

/// 系统 Toast（尽力而为）：经 PowerShell 调 Windows.UI.Notifications，
/// 无需任何第三方模块；失败静默（GUI 内横幅是主通道）。
pub fn toast(title: &str, body: &str) {
    let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('\'', "''");
    let title = esc(title);
    let body = esc(body);
    let script = format!(
        "$ErrorActionPreference='SilentlyContinue';\
         [Windows.UI.Notifications.ToastNotificationManager,Windows.UI.Notifications,ContentType=WindowsRuntime]|Out-Null;\
         [Windows.Data.Xml.Dom.XmlDocument,Windows.Data.Xml.Dom.XmlDocument,ContentType=WindowsRuntime]|Out-Null;\
         $x=New-Object Windows.Data.Xml.Dom.XmlDocument;\
         $x.LoadXml('<toast><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual></toast>');\
         $t=[Windows.UI.Notifications.ToastNotification]::new($x);\
         [Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier('SteamSweep').Show($t)",
        title, body
    );
    let _ = std::process::Command::new("powershell")
        .args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", &script])
        .spawn();
}

/// 供 channel 场景使用的便捷启动：卸载事件转为消息发送。
pub fn start_to_channel<T: Send + 'static>(
    loc: &Location,
    prev: BTreeMap<u32, String>,
    tx: mpsc::SyncSender<T>,
    wrap: impl Fn(u32, String) -> T + Send + 'static,
    ctx: egui::Context,
) {
    start(loc, prev, move |appid, name| {
        toast(
            &format!("已卸载《{}》", name),
            "可能有安装残留和存档可以清理，打开 SteamSweep 查看",
        );
        let _ = tx.send(wrap(appid, name));
        ctx.request_repaint();
    });
}
