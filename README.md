# PetPom · 桌宠 + 番茄钟

一只常驻屏幕角落的像素小动物 + 一个番茄钟。到点后它用动作和气泡提醒你休息，你可以「摸摸它」让它再等 5 分钟。

V0 只验证一件事：**情感化提醒是否真的比弹窗提醒更容易被接受**。

---

## 快速开始（1 分钟，无需编译）

### 系统要求

| 项 | 要求 |
|---|---|
| 系统 | **macOS**（暂不支持 Windows / Linux） |
| 芯片 | **Apple Silicon 或 Intel 均可**（Release 是 Universal 通用二进制） |
| 其他 | 无需网络、无需账号；数据只存本机 |

### 安装

1. 打开 [Releases](https://github.com/treatortrick/petpom/releases) 下载 `PetPom-macos-universal.zip`（约 11 MB）
2. 解压，把 `PetPom.app` 拖进「应用程序」
3. **首次打开会被系统拦截**（应用没有 Apple 开发者签名）：
   - 在「应用程序」里**右键**点 `PetPom.app` → 选「**打开**」→ 弹窗里再点一次「打开」
   - 或者执行一次命令解除隔离：
     ```bash
     xattr -dr com.apple.quarantine /Applications/PetPom.app
     ```
4. 启动后：菜单栏出现图标，桌宠出现在屏幕**右下角**

---

## 使用方法

### 1. 开始一轮专注

点**菜单栏图标** → 「开始专注」。默认 25 分钟（可在设置里改）。

专注时宠物会捧着书看，头顶显示倒计时：

| 状态 | 时间牌表现 |
|---|---|
| 专注中 | 橙色圆点缓慢呼吸 |
| 休息中 | 青绿点 |
| 已暂停 | 灰点静止（点时间牌即可暂停/继续） |
| 最后 1 分钟 | 红点快闪 |

### 2. 到点提醒

时间到了会弹出提醒胶囊（窗口会自动展开，宠物站在旁边）：

- **好，休息** → 进入休息，宠物趴下睡觉
- **摸摸它再等 5 分钟** → 推迟 5 分钟（右键点宠物 1 秒等效）
- **跳过本轮** → 本轮作废
- **60 秒不理会** → 自动收起，记为「忽略」

### 3. 投喂出门（可选玩法）

完成一轮专注得 **+1 小鱼干**（每天早上第一次打开也会送 1 条，上限 9 条）。

投喂后它背着小布包出门 **12~28 分钟**（关掉 app 也在走时间），回来一定带回一张**卡片**，收进「设置 → 它的小窝」。卡片共 21 张，抽卡优先给还没收集过的。

> 这个机制刻意做到**零压力**：不投喂不会有任何惩罚，没有饥饿值、没有断签。也可以彻底关掉（设置 → 允许投喂出门探险）。
> 专注进行中它回来时**只播动画、不弹卡片**——不打扰你。

### 4. 日常交互

| 操作 | 效果 |
|---|---|
| 点它 | 抚摸（换动作 + 气泡） |
| 按住拖 | 换位置（松手后自动贴回屏内，不会拖丢） |
| 右键 | 菜单：暂停 / 跳过 / 投喂 / 叫它回来 / 换形象 / 鼠标穿透 / 设置 / 隐藏 / 退出 |
| 鼠标移到空白处 | 点击**穿透**到后面的应用（只有宠物轮廓和时间牌响应） |

### 5. 设置项

菜单栏图标 → 「设置…」

- 专注时长 / 短休息 / 长休息间隔
- 形象（奶油柴犬 / 团团·橘猫）、宠物名字
- 专注时隐藏气泡
- 允许投喂出门探险
- 鼠标穿透（纯观赏模式）
- 开机自启
- 「它的小窝」：卡片收集进度

---

## 数据存在哪

```
~/Library/Application Support/com.dashu.petpom/petpom.db
```

设置、专注记录、卡片、探险进度全在这一个 SQLite 文件里。**删掉这个目录 = 恢复出厂设置**。

---

## 常见问题

**Q：打不开，提示「无法验证开发者」？**
应用未签名。右键 → 打开 → 再点「打开」；或 `xattr -dr com.apple.quarantine /Applications/PetPom.app`。

**Q：Intel Mac 能用吗？**
能。Release 提供 **Universal 二进制**（同时包含 arm64 与 x86_64），M 系列和 Intel 都能直接跑。

**Q：点了宠物没反应？**
两个可能：①菜单里开了「鼠标穿透」，关掉即可；②点在了它周围的透明区域—— **只有可见的宠物轮廓和时间牌才算热区**，其余区域点击会穿透给后面的应用。

**Q：宠物跑到屏幕外/找不到了？**
重启应用会自动夹回屏幕内；或用菜单栏图标 → 「显示桌宠」。

**Q：为什么没有声音？**
语音功能已于 2026-09-15 整体下架，当前全部静音，只保留系统通知。

**Q：卡片一直是重复的？**
抽卡会优先给未收集的；21 张集满后才会重复。

**Q：接外接显示器后拖动异常？**
已支持多屏（窗口坐标用物理像素并自动夹回当前屏）。如遇问题，把桌宠拖回主屏一次即可。

---

## 从源码构建

需要 **Rust**（rustup）与 **Node**（只用 `@tauri-apps/cli`）：

```bash
git clone https://github.com/treatortrick/petpom.git
cd petpom
npm install
npx tauri build      # 约 3 分钟，产物在 src-tauri/target/release/bundle/macos/

# 打 Apple Silicon + Intel 通用包（需先装另一架构的 target）
rustup target add x86_64-apple-darwin
npx tauri build --target universal-apple-darwin
```

开发调试可直接跑：

```bash
npm install && npx tauri dev
```

调试期把数据落在项目内便于清理：

```bash
PETPOM_DATA_DIR=$PWD/data ./src-tauri/target/debug/petpom
```

---

## 仓库结构

```
ui/                     前端（原生 HTML/JS/CSS，无构建步骤）
  index.html            宠物窗口：sprite 容器 + 时间牌 + 气泡 + 提醒胶囊 + 卡片浮层
  pet.js                宠物引擎：动画状态机、拖拽、命中掩码、探险卡片
  settings.html/.js     设置页
  assets/petpacks/
    shiba/              奶油柴犬（18 个动作 / 85 帧）
    tuantuan/           团团 · 橘猫（18 个动作 / 85 帧）
      manifest.json     动画元数据：帧率、循环、状态映射、命中掩码 —— 唯一真相源
      manifest.js       运行期副本（WebView 不能 fetch 自定义协议）

src-tauri/
  src/main.rs           托盘与菜单、窗口编排、探险与召回、命中测试、位置夹取
  src/timer.rs          番茄钟引擎（独立线程，含休眠唤醒检测与无效轮次判定）
  src/db.rs             SQLite：settings / rounds / expeditions / cards
  tauri.conf.json       窗口 255×235、acceptFirstMouse、bundle 配置
  icons/                应用图标
  Cargo.lock            已入库，保证可复现构建

assets/                 动画资产生产管线（抽帧 / 抠底 / 量化）
```

---

## 已知限制

1. **仅 macOS**：`bundle.targets = ["app"]`，窗口的 `acceptFirstMouse` 也是 macOS 专用。Windows / Linux 未适配（架构已覆盖 Intel + Apple Silicon）
2. **未签名、未公证**：首次打开必须手动绕过 Gatekeeper；跨机器分发时系统会重置隔离标记
3. **无法从零重建动画资产**：管线脚本在仓库里，但源视频（约 340 MB）不在。可以处理你自己的视频，但无法复现现有的 170 帧
4. **无 CI / 自动发布**：Releases 由本地构建后手动上传
5. 语音功能已下架；交互与动画规格文档未包含在仓库中

---

## 许可证

[MIT](LICENSE) © 2026 Allen —— 可自由使用、修改、再分发，需保留版权声明。
