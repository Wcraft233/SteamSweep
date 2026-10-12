# SteamSweep

Steam 残留清理二合一工具：**已卸载游戏安装残留清理** + **存档管理/清理**。

> 全部里程碑（M1~M5）已完成。所有删除只进系统回收站，随时可恢复；云端存档永不被修改。

## 功能

- **零配置自动发现**：注册表定位 Steam → 解析 `libraryfolders.vdf` 全部库 → `loginusers.vdf` ∪ `userdata` 全账号
- **安装残留**：`common` 无主文件夹 diff（含 `steam_autocloud.vdf` 黄金标记真伪判别）、`downloading` / `workshop\content` / `shadercache` 残留；目录大小全部 rayon 并行计算
- **存档管理**：逐账号逐 App 解析 `remotecache.vdf` 云存档索引，Root 编号映射（0/1/2/3/6/9/11/12）展开本地真实路径并检测文件是否仍存在；`syncstate` 三色判定（🔴 疑似从未上传 / 🟡 最近进度或未上传 / 已同步）；空壳目录、`remote\` 有文件但索引丢失、`ac\` 骨架识别
- **存档定位补全**：解析 `appinfo.vdf`（v29 二进制）的 `ufs/savefiles` 块，补定位未出现在 userdata 中的游戏；离线 SaveDB（190+ 游戏存档路径后缀）按目录归属；支持自定义额外扫描目录
- **共性痕迹**（快速版）：`steam_autocloud.vdf` 黄金标记扫描（限 AppData / Documents / Saved Games 等常见目录，OneDrive 占位文件防卡死）+ `appcache\stats` 成就痕迹统计
- **清理**：勾选 → 确认（高风险存档红色警示）→ 全部进系统回收站 + JSON 操作日志 → 自动重扫
- **卸载联动提醒**：快照 diff 监听 acf 变化，游戏卸载时 Toast + 应用内横幅提醒清理存档与残留，可"不再提醒此游戏"
- **扫描缓存**：目录体积结果落盘，重复扫描走增量校验，二次启动秒出结果
- **GUI**：eframe/egui 原生界面，Steam 风深色主题 + 左侧导航；大封面图 + 大字游戏名 + 复选框 + 按大小排序 + 分组折叠（可安全删除 / 仅本地痕迹 / 有丢失风险 / 未卸载默认折叠）；游戏名中文优先（商店页抓取 + 全量名称表离线匹配）、账号昵称与头像、封面三级回退（本地 → CDN → 占位插画）、空状态插画；单实例互斥（二次启动直接唤出已有窗口）；运行时组件（Proton / 运行库）默认排除
- **CLI**：`steamsweep --cli` 输出终端摘要 + `steamsweep-report.json` 结构化报告
- **设置**：可选本地代理（Steam 接口被阻断时兜底）、Steam API 密钥（补全游戏名）、额外扫描目录，配置仅存本机

界面 logo、空状态插画、封面占位图由 AI 生成。

## 构建

```bash
cargo build --release
```

## 使用

```bash
steamsweep          # GUI（启动即自动逐步扫描）
steamsweep --cli    # 控制台报告 + steamsweep-report.json
```

## 路线图

| 阶段 | 内容 | 状态 |
| --- | --- | --- |
| M1 扫描器 | locator + manifest + orphan + savemap + 快速痕迹，GUI/CLI 报告 | ✅ |
| M2 清理 | 回收站删除、操作日志、确认/进度/结果界面 | ✅ |
| M3 存档定位完善 | appinfo.vdf ufs 解析、SaveDB 离线数据库、自定义扫描目录 | ✅ |
| M4 卸载联动提醒 | acf 快照 diff、Toast + 横幅提醒、防打扰 | ✅ |
| M5 界面 | Steam 风深色主题、左侧导航、AI 生成美术资产、空状态/封面占位 | ✅ |

## 相关仓库

- [save-db](https://github.com/Wcraft233/save-db)：众包存档路径数据库（SaveDB），客户端冷启动数据即来自本机实测。

## License

MIT
