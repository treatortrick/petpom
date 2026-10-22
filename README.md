# PetPom · 桌宠 + 番茄钟

一只常驻屏幕角落的像素小动物 + 一个番茄钟。到点后它用动作和气泡提醒你休息，你可以「摸摸它」让它再等 5 分钟。

V0 只验证一件事：**情感化提醒是否真的比弹窗提醒更容易被接受**。

## 技术栈

- **Tauri 2**：Rust 后端 + 系统 WebView 前端。前端是原生 HTML/JS/CSS，无构建步骤
- **数据**：本地 SQLite（`app_data_dir/petpom.db`），无账号、无云同步
- **平台**：macOS（当前只打包 `.app`，不出 DMG）
- 语音已于 2026-09-15 整体下架（留待后续迭代），当前运行全部静音

## 构建与运行

```bash
# 依赖：Rust (rustup) + Node（仅用 @tauri-apps/cli）
npm install
npx tauri dev     # 开发运行
npx tauri build   # 打包 .app → src-tauri/target/release/bundle/macos/
```

开发期把数据落到项目内便于清理：

```bash
PETPOM_DATA_DIR=$PWD/data ./src-tauri/target/debug/petpom
```

正式运行数据在 `~/Library/Application Support/com.dashu.petpom/`。

## 仓库结构

```
ui/                     前端
  index.html            宠物窗口：sprite 容器 + 时间牌 + 气泡 + 提醒胶囊 + 卡片浮层
  pet.js                宠物引擎：动画状态机、拖拽、命中掩码、探险卡片
  settings.html/.js     设置页（首次打开时懒创建）
  assets/petpacks/
    shiba/              奶油柴犬（18 个动作 / 85 帧）
    tuantuan/           团团 · 橘猫（18 个动作 / 85 帧）
      manifest.json     动画元数据：帧率、循环、状态映射、命中掩码 —— 唯一真相源
      manifest.js       运行期副本（WebView 不能 fetch 自定义协议，只能 <script> 加载）

src-tauri/
  src/main.rs           托盘与菜单、窗口编排、探险与召回、命中测试、位置夹取
  src/timer.rs          番茄钟引擎（独立线程，含休眠唤醒检测与无效轮次判定）
  src/db.rs             SQLite：settings / rounds / expeditions / cards
  tauri.conf.json       窗口 255×235、acceptFirstMouse、bundle 配置
  icons/                应用图标（32 / 128 / icns + 母版）
  Cargo.lock            已入库，保证可复现构建
```

## 几个设计约束（改代码前值得知道）

- **精灵帧是编译期硬依赖**：`main.rs` 用 `include_bytes!` 引用 4 张做托盘图标，前端运行时加载全部 170 帧。删掉资源会导致 `cargo build` 失败、App 无图形
- **动画稳态只由 phase 驱动**：`timer/tick` 把阶段推给前端，前端用一张映射表决定形态（`focus→看书`、`rest→睡觉`、`focus+paused→坐下`）。菜单命令不各自发形态事件——否则新增一条路径就会漏（曾漏 6 条）
- **热区按可见轮廓命中**：窗口整块透明，靠 `set_ignore_cursor_events` 动态切换实现真穿透；轮廓掩码在构建期离线算好写进 `manifest.hit_mask`

## 不在本仓库的内容

- 动画资产生产管线（抽帧 / 抠底 / 量化）与交互规格文档 —— 仅本地保留
- 开发辅助脚本 —— 仅本地保留

## 说明

本项目为闭源私有工程，未附带开源许可证。
