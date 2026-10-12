# SteamSweep

Steam 残留清理二合一工具：**已卸载游戏安装残留清理** + **存档管理/清理**。

> 当前里程碑：**M1 只读扫描器**（GUI + CLI）。全程只读，不修改、不删除、不移动任何文件。
> 红线：永不触碰云端存档；未来的删除操作（M2）只作用于本地且默认进回收站。

## 功能（M1）

- **零配置自动发现**：注册表定位 Steam → 解析 `libraryfolders.vdf` 全部库 → `loginusers.vdf` ∪ `userdata` 全账号
- **安装残留**：`common` 无主文件夹 diff（含 `steam_autocloud.vdf` 黄金标记真伪判别）、`downloading` / `workshop\content` / `shadercache` 残留；目录大小全部 rayon 并行计算
- **存档管理**：逐账号逐 App 解析 `remotecache.vdf` 云存档索引，Root 编号映射（0/1/2/3/6/9/11/12）展开本地真实路径并检测文件是否仍存在；`syncstate` 三色判定（🔴 疑似从未上传 = 删除即永久丢失 / 🟡 最近进度或未上传 / 已同步）；空壳目录、`remote\` 有文件但索引丢失、`ac\` 骨架识别
- **共性痕迹**（快速版）：`steam_autocloud.vdf` 黄金标记全盘扫描（限 AppData / Documents / Saved Games 等常见目录，OneDrive 占位文件防卡死）+ `appcache\stats` 成就痕迹统计
- **GUI**：eframe/egui 原生深色界面，小白清理工具风格——大封面图 + 大字游戏名 + 复选框 + 按大小排序 + 分组折叠（可安全删除 / 仅本地痕迹 / 有丢失风险 / 未卸载）；游戏名中文优先（商店页抓取 + 全量名称表离线匹配）、账号昵称与头像、单实例互斥、运行时组件（Proton / 运行库）默认排除
- **CLI**：`steamsweep --cli` 输出终端摘要 + `steamsweep-report.json` 结构化报告
- **设置**：可选本地代理（Steam 接口被网络阻断时兜底）、Steam API 密钥（补全游戏名），配置仅存本机

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
| M2 清理 | 回收站删除、备份打包、操作日志、syncstate=2 强制备份 | 进行中 |
| M3 存档定位完善 | 完整启发式（引擎特征/扩展名规则，代码已备）、appinfo.vdf ufs 解析、SaveDB 众包数据库、PCGamingWiki 兜底 | 未开始 |
| M4 卸载联动提醒 | acf 监听 + 快照 diff、Toast 提醒、防打扰 | 未开始 |

## 相关仓库

- [save-db](https://github.com/Wcraft233/save-db)：众包存档路径数据库（SaveDB），客户端冷启动数据即来自本机实测。

## License

MIT
