#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod db;
mod timer;

use db::Db;
use serde::Serialize;
use serde_json::Value;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use tauri::menu::{MenuItemBuilder};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder, Wry};

const PET_SIZE: (f64, f64) = (255.0, 315.0);
const ALERT_SIZE: (f64, f64) = (765.0, 420.0);
const ALERT_GROW: f64 = ALERT_SIZE.0 - PET_SIZE.0; // 左扩，保持右缘不动

#[derive(Clone, Serialize)]
struct Settings {
    focus_min: i64,
    short_min: i64,
    long_every: i64,
    long_min: i64,
    pet_pack: String,
    click_through: bool,
    autostart: bool,
    hide_bubble: bool,
    /// 用户自定义名；空串 = 用形象默认名（见 interaction-spec.md §3）
    pet_name: String,
    /// 零食库存：完成一轮专注 +1，上限 9，不衰减
    snacks: i64,
    /// 亲密度：累计投喂次数，只增不减、无惩罚（拒绝养成衰减那套）
    bond: i64,
    pet_x: f64,
    pet_y: f64,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            focus_min: 25,
            short_min: 5,
            long_every: 4,
            long_min: 15,
            pet_pack: "shiba".into(),
            click_through: false,
            autostart: false,
            hide_bubble: false,
            pet_name: String::new(),
            snacks: 0,
            bond: 0,
            pet_x: -1.0,
            pet_y: -1.0,
        }
    }
}

impl Settings {
    fn load(db: &Db) -> Self {
        let mut s = Settings::default();
        let from_db = |key: &str, f: &mut dyn FnMut(&str)| {
            if let Some(v) = db.get_setting(key) {
                f(&v);
            }
        };
        from_db("focus_min", &mut |v| s.focus_min = v.parse().unwrap_or(s.focus_min));
        from_db("short_min", &mut |v| s.short_min = v.parse().unwrap_or(s.short_min));
        from_db("long_every", &mut |v| s.long_every = v.parse().unwrap_or(s.long_every));
        from_db("long_min", &mut |v| s.long_min = v.parse().unwrap_or(s.long_min));
        from_db("pet_pack", &mut |v| s.pet_pack = v.to_string());
        from_db("click_through", &mut |v| s.click_through = v == "true");
        from_db("autostart", &mut |v| s.autostart = v == "true");
        from_db("hide_bubble", &mut |v| s.hide_bubble = v == "true");
        from_db("pet_name", &mut |v| s.pet_name = v.to_string());
        from_db("snacks", &mut |v| s.snacks = v.parse().unwrap_or(s.snacks));
        from_db("bond", &mut |v| s.bond = v.parse().unwrap_or(s.bond));
        from_db("pet_x", &mut |v| s.pet_x = v.parse().unwrap_or(s.pet_x));
        from_db("pet_y", &mut |v| s.pet_y = v.parse().unwrap_or(s.pet_y));
        s
    }

    fn save_key(&self, db: &Db, key: &str) {
        let v = match key {
            "focus_min" => self.focus_min.to_string(),
            "short_min" => self.short_min.to_string(),
            "long_every" => self.long_every.to_string(),
            "long_min" => self.long_min.to_string(),
            "pet_pack" => self.pet_pack.clone(),
            "click_through" => self.click_through.to_string(),
            "autostart" => self.autostart.to_string(),
            "hide_bubble" => self.hide_bubble.to_string(),
            "pet_name" => self.pet_name.clone(),
            "snacks" => self.snacks.to_string(),
            "bond" => self.bond.to_string(),
            "pet_x" => self.pet_x.to_string(),
            "pet_y" => self.pet_y.to_string(),
            _ => return,
        };
        db.set_setting(key, &v);
    }
}

struct AppState {
    db: Db,
    settings: Mutex<Settings>,
    timer: timer::TimerHandle,
    alerting: AtomicBool,
    last_saved_pos: Mutex<(f64, f64)>,
    /// 托盘状态行句柄（每秒更新倒计时文本）
    tray_status: Mutex<Option<tauri::menu::MenuItem<Wry>>>,
    /// (phase, paused, urgent, remain) —— urgent: 专注且剩 <60s（图标换 alert 帧）
    ui_phase: Mutex<(String, bool, bool, u64)>,
    /// 桌宠隐藏（会话级，不持久化；提醒到点强制弹现）
    hidden: AtomicBool,
    /// 专注反应 once 标志（interaction-spec #3/#4），进入新一轮专注时重置
    react_half: AtomicBool,
    react_final: AtomicBool,
    /// 本轮专注中被点击（开小差）的次数：进入新一轮专注时清零，结算时读取
    distractions: std::sync::atomic::AtomicU64,
}

// ---------- 宠物命名 / 文案 ----------

fn default_pack_name(pack: &str) -> &'static str {
    match pack {
        "tuantuan" => "团团",
        _ => "奶油",
    }
}

fn pet_display_name(s: &Settings) -> String {
    let n = s.pet_name.trim();
    if n.is_empty() {
        default_pack_name(&s.pet_pack).to_string()
    } else {
        n.to_string()
    }
}

/// 气泡模板：{n} = 宠物名
fn named(tmpl: &str, s: &Settings) -> String {
    tmpl.replace("{n}", &pet_display_name(s))
}

fn pick<'a>(v: &[&'a str]) -> &'a str {
    use std::time::{SystemTime, UNIX_EPOCH};
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    v[(n as usize) % v.len()]
}

fn phase_label(phase: &str, paused: bool, remain: u64) -> String {
    match phase {
        "focus" => {
            if paused {
                format!("已暂停 · {}", fmt_mmss(remain))
            } else {
                format!("专注中 · {}", fmt_mmss(remain))
            }
        }
        "rest" => format!("休息中 · {}", fmt_mmss(remain)),
        "alert-focus" | "alert-rest" => "到点啦 · 等你响应".to_string(),
        _ => "空闲中".to_string(),
    }
}

// ---------- 提醒 UI 编排（窗口扩缩 + 事件） ----------

fn fmt_mmss(sec: u64) -> String {
    format!("{:02}:{:02}", sec / 60, sec % 60)
}

fn pet_window(app: &AppHandle) -> Option<WebviewWindow<Wry>> {
    app.get_webview_window("pet")
}

/// 手动拖拽：前端按指针位移算出新窗口坐标发过来，这里直接落位。
/// 不用系统的 startDragging —— macOS 上它在 pointermove 里调用会有
/// 陈旧 mouseDown 锚点 / 拖拽会话吞掉 mouseup 的问题（只能拖一次/拖不远）。
#[tauri::command]
fn drag_move(app: AppHandle, x: f64, y: f64) {
    if let Some(w) = pet_window(&app) {
        let _ = w.set_position(LogicalPosition::new(x, y));
    }
}

/// 设置窗口懒创建（内存优化，2026-09-22）：
/// 原静态声明（visible:false）导致启动即常驻一个 WebView（约 38MB）。
/// 改为首次点「设置…」时才创建；关闭 = 隐藏而非销毁（复用窗口与 webview 状态）。
fn open_settings(app: &AppHandle) -> tauri::Result<()> {
    if let Some(w) = app.get_webview_window("settings") {
        let _ = w.show();
        let _ = w.set_focus();
        return Ok(());
    }
    let w = WebviewWindowBuilder::new(app, "settings", WebviewUrl::App("settings.html".into()))
        .title("设置 · PetPom")
        .inner_size(580.0, 660.0)
        .resizable(false)
        .build()?;
    // 关闭 = 隐藏而非销毁：否则系统红叉/Cmd+W 会销毁 webview，
    // 之后 get_webview_window 返回 None，设置页就永远打不开了
    let w2 = w.clone();
    w.on_window_event(move |event| {
        if let tauri::WindowEvent::CloseRequested { api, .. } = event {
            api.prevent_close();
            let _ = w2.hide();
        }
    });
    Ok(())
}

fn show_alert_ui(app: &AppHandle, kind: &str, limited: bool) {
    let Some(state) = app.try_state::<AppState>() else { return };
    let settings = state.settings.lock().unwrap().clone();
    if let Some(w) = pet_window(app) {
        let _ = w.show(); // 隐藏态也要弹现提醒（interaction-spec 红线）
        let _ = w.set_size(LogicalSize::new(ALERT_SIZE.0, ALERT_SIZE.1));
        let _ = w.set_position(LogicalPosition::new(
            settings.pet_x - ALERT_GROW,
            settings.pet_y - 40.0,
        ));
    }
    state.alerting.store(true, Ordering::SeqCst);
    let _ = app.emit(
        "alert/show",
        serde_json::json!({ "kind": kind, "limited": limited, "hide_bubble": settings.hide_bubble, "focus_min": settings.focus_min }),
    );
    if kind == "focus-end" {
        // 完成庆祝（interaction-spec #5）
        let _ = app.emit(
            "pet/bubble",
            serde_json::json!({ "text": named("{n}陪你完成了一轮专注，干得漂亮！", &settings), "ms": 4000 }),
        );
    }
}

fn hide_alert_ui(app: &AppHandle, pet_state: &str) {
    let Some(state) = app.try_state::<AppState>() else { return };
    let settings = state.settings.lock().unwrap().clone();
    if let Some(w) = pet_window(app) {
        let _ = w.set_size(LogicalSize::new(PET_SIZE.0, PET_SIZE.1));
        let _ = w.set_position(LogicalPosition::new(settings.pet_x, settings.pet_y));
        if state.hidden.load(Ordering::SeqCst) {
            let _ = w.hide(); // 隐藏态提醒结束：继续隐身
        }
    }
    state.alerting.store(false, Ordering::SeqCst);
    let _ = app.emit("alert/hide", {});
    if !pet_state.is_empty() {
        let _ = app.emit("pet/state", serde_json::json!({ "state": pet_state }));
    }
}

// ---------- 托盘 ----------

/// 取当前形象包某动画的首帧 PNG 字节（托盘图标用）。
/// 依次尝试：打包资源目录 → 开发期源码目录 → 编译期内置的柴犬帧。
fn pack_icon_bytes(app: &AppHandle, pack: &str, anim: &str) -> Vec<u8> {
    let rel = format!("ui/assets/petpacks/{pack}/anims/{anim}/{anim}-01.png");
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(rd) = app.path().resource_dir() {
        candidates.push(rd.join(&rel));
        candidates.push(rd.join("_up_").join(&rel)); // macOS .app 内资源目录的兜底布局
    }
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("../").join(&rel));
        candidates.push(cwd.join(&rel));
    }
    for p in candidates {
        if let Ok(b) = std::fs::read(&p) {
            if !b.is_empty() {
                return b;
            }
        }
    }
    // 兜底：编译期内置的柴犬帧（保证托盘永远有图标）
    let fallback: &[u8] = match anim {
        "alert" => include_bytes!("../../ui/assets/petpacks/shiba/anims/alert/alert-01.png"),
        "sleep" => include_bytes!("../../ui/assets/petpacks/shiba/anims/sleep/sleep-01.png"),
        "focus-read" => include_bytes!("../../ui/assets/petpacks/shiba/anims/focus-read/focus-read-01.png"),
        _ => include_bytes!("../../ui/assets/petpacks/shiba/anims/walk/walk-01.png"),
    };
    fallback.to_vec()
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let state = app.state::<AppState>();
    let s = state.settings.lock().unwrap().clone();
    let (phase, paused, urgent, _remain) = state.ui_phase.lock().unwrap().clone();
    let running = matches!(phase.as_str(), "focus" | "rest" | "alert-focus" | "alert-rest");
    let hidden = state.hidden.load(Ordering::SeqCst);

    // 防重复图标：先移除旧托盘再重建
    let _ = app.remove_tray_by_id("main");

    let status_text = phase_label(&phase, paused, _remain);
    let status = tauri::menu::MenuItemBuilder::with_id("status", status_text)
        .enabled(false)
        .build(app)?;
    *state.tray_status.lock().unwrap() = Some(status.clone());

    // 图标随状态换（设计稿：专注=看书帧、休息=睡觉帧、剩 <1 分钟=提醒帧）。
    // 2026-09-23 修复：此前硬编码 shiba 包，选了猫形象托盘里还是狗。
    // 改为运行时按当前形象包读图；读不到时回退到编译期内置的柴犬帧。
    let pack = s.pet_pack.as_str();
    let icon_anim = match (phase.as_str(), paused, urgent) {
        ("focus", false, true) => "alert",
        ("focus", _, _) => "focus-read",
        ("rest", _, _) => "sleep",
        ("alert-focus", _, _) | ("alert-rest", _, _) => "alert",
        _ => "walk",
    };
    let icon_buf = pack_icon_bytes(app, pack, icon_anim);
    let icon_bytes: &[u8] = &icon_buf;

    let (toggle, skip) = if running {
        let skip_label = match phase.as_str() {
            "rest" => "跳过这次休息",
            "alert-rest" => "跳过这次休息",
            _ => "跳过这次专注",
        };
        (
            Some(tauri::menu::MenuItemBuilder::with_id(
                "pause_resume",
                if paused {
                    if phase == "rest" { "继续休息" } else { "继续专注" }
                } else if phase == "rest" {
                    "暂停休息"
                } else {
                    "暂停一下"
                },
            ).build(app)?),
            Some(tauri::menu::MenuItemBuilder::with_id("skip", skip_label).build(app)?),
        )
    } else {
        (
            Some(tauri::menu::MenuItemBuilder::with_id("start", "开始专注").build(app)?),
            None,
        )
    };

    let feed = tauri::menu::MenuItemBuilder::with_id(
        "feed",
        format!("投喂 · 小鱼干 ×{}", s.snacks),
    )
    .enabled(s.snacks > 0 && !matches!(phase.as_str(), "alert-focus" | "alert-rest"))
    .build(app)?;
    // 今日战绩行（不可点，纯信息）：与结算卡同源
    let (today_rounds, today_min) = state.db.stats_today();
    let stats_line = tauri::menu::MenuItemBuilder::with_id(
        "stats_today",
        format!("今日 🍅 ×{} · 专注 {} 分钟", today_rounds, today_min),
    )
    .enabled(false)
    .build(app)?;
    let show_pet = tauri::menu::CheckMenuItemBuilder::with_id("show_pet", "显示桌宠")
        .checked(!hidden)
        .build(app)?;

    let clickthrough = tauri::menu::CheckMenuItemBuilder::with_id("clickthrough", "鼠标穿透")
        .checked(s.click_through)
        .build(app)?;
    let pack_cat = tauri::menu::CheckMenuItemBuilder::with_id("pack_tuantuan", "形象：团团 · 橘猫")
        .checked(s.pet_pack == "tuantuan")
        .build(app)?;
    let pack_shiba = tauri::menu::CheckMenuItemBuilder::with_id("pack_shiba", "形象：奶油柴犬")
        .checked(s.pet_pack == "shiba")
        .build(app)?;
    let settings_item = MenuItemBuilder::with_id("settings", "设置…").build(app)?;
    let sep2 = tauri::menu::PredefinedMenuItem::separator(app)?;
    let quit = MenuItemBuilder::with_id("quit", "退出").build(app)?;

    let sep_a = tauri::menu::PredefinedMenuItem::separator(app)?;
    let sep_b = tauri::menu::PredefinedMenuItem::separator(app)?;
    let sep_c = tauri::menu::PredefinedMenuItem::separator(app)?;
    let mut items: Vec<&dyn tauri::menu::IsMenuItem<Wry>> = vec![&status, &stats_line, &sep_a];
    if let Some(t) = &toggle { items.push(t); }
    if let Some(sk) = &skip { items.push(sk); }
    items.push(&sep_b);
    items.push(&feed);
    items.push(&show_pet);
    items.push(&sep_c);
    items.push(&clickthrough);
    items.push(&pack_cat);
    items.push(&pack_shiba);
    items.push(&settings_item);
    items.push(&sep2);
    items.push(&quit);
    let menu = tauri::menu::MenuBuilder::new(app).items(&items).build()?;

    let icon = tauri::image::Image::from_bytes(icon_bytes)?;

    TrayIconBuilder::with_id("main")
        .icon(icon)
        .icon_as_template(false)
        .tooltip(format!("PetPom · {}", pet_display_name(&s)))
        .menu(&menu)
        .show_menu_on_left_click(true)
        .build(app)?;
    Ok(())
}

// 在菜单事件回调里同步重建托盘在 macOS 上易卡死主线程（AppKit 菜单跟踪循环），
// 统一改为：短暂延迟后经事件循环重建
fn rebuild_tray_later(app: &AppHandle) {
    let a = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(150));
        let b = a.clone();
        let _ = a.run_on_main_thread(move || {
            let _ = build_tray(&b);
        });
    });
}
/// 全局唯一菜单事件分发器：托盘菜单与宠物右键菜单共用（Builder.on_menu_event 注册）。
/// 注意：不能用 TrayIconBuilder::on_menu_event —— 它每次托盘重建都会往全局监听表
/// 再压一个 handler 且从不移除，运行久了点一次「暂停」会被处理 N 次。
fn on_menu_action(app: &AppHandle, id: &str) {
    let state = app.state::<AppState>();
    match id {
        "start" => {
            let min = state.settings.lock().unwrap().focus_min as u64;
            let _ = state.timer.0.send(timer::Cmd::StartFocus { min });
            state.distractions.store(0, Ordering::SeqCst); // 新一轮：开小差计数归零
            let _ = app.emit("pet/state", serde_json::json!({ "state": "focus" }));
            let s = state.settings.lock().unwrap().clone();
            let _ = app.emit("pet/bubble", serde_json::json!({ "text": named("要开始专注了嘛？{n}陪你一起看书～", &s), "ms": 4000 }));
        }
        "pause_resume" => {
            let _ = state.timer.0.send(timer::Cmd::PauseResume);
        }
        "skip" => {
            let _ = state.timer.0.send(timer::Cmd::Skip);
        }
        "feed" => feed_pet_internal(app),
        "show_pet" => {
            let hide = state.hidden.load(Ordering::SeqCst); // 勾选=显示 → 取消隐藏
            set_pet_hidden(app, !hide);
            if hide {
                let s = state.settings.lock().unwrap().clone();
                let _ = app.emit("pet/bubble", serde_json::json!({ "text": named("{n}回来啦！", &s), "ms": 3000 }));
            }
        }
        "hide_pet" => set_pet_hidden(app, true),
        "clickthrough" => {
            {
                let mut s = state.settings.lock().unwrap();
                s.click_through = !s.click_through;
                s.save_key(&state.db, "click_through");
                if let Some(w) = pet_window(app) {
                    let _ = w.set_ignore_cursor_events(s.click_through);
                }
            } // 先释放锁再重建，build_tray 内部也要拿这把锁，持锁重建会死锁
            rebuild_tray_later(app);
        }
        "pack_tuantuan" | "pack_shiba" => {
            let pack = if id == "pack_tuantuan" { "tuantuan" } else { "shiba" };
            {
                let mut s = state.settings.lock().unwrap();
                if s.pet_pack != pack {
                    s.pet_pack = pack.into();
                    s.save_key(&state.db, "pet_pack");
                }
            }
            let _ = app.emit("pet/set-pack", serde_json::json!({ "pack": pack }));
            rebuild_tray_later(app);
        }
        "settings" => {
            if let Err(e) = open_settings(app) {
                eprintln!("[petpom] open settings window failed: {e}");
            }
        }
        "quit" => {
            app.exit(0);
        }
        "status" | "pause" => {}
        _ => {}
    }
}

/// 隐藏/显示桌宠（不退出，计时照跑；提醒到点强制弹现）
fn set_pet_hidden(app: &AppHandle, hide: bool) {
    let state = app.state::<AppState>();
    state.hidden.store(hide, Ordering::SeqCst);
    if let Some(w) = pet_window(app) {
        let _ = if hide { w.hide() } else { w.show() };
    }
    rebuild_tray_later(app);
}

/// 投喂：零食 -1、亲密度 +1，播吃动画（eat 资产未到位时前端自动回退 happy）+ 谢投喂气泡
fn feed_pet_internal(app: &AppHandle) {
    let state = app.state::<AppState>();
    {
        let mut s = state.settings.lock().unwrap();
        if s.snacks <= 0 {
            return;
        }
        s.snacks -= 1;
        s.bond += 1; // 亲密度只增不减，无衰减惩罚
        s.save_key(&state.db, "snacks");
        s.save_key(&state.db, "bond");
    }
    let settings = state.settings.lock().unwrap().clone();
    let text = pick(&[
        "谢谢投喂，{n}开动啦！",
        "呜哇，是小鱼干！{n}最喜欢了～",
        "为了零食，下一轮也要加油！",
        "好吃到转圈圈…（吧唧吧唧）",
    ]);
    let _ = app.emit("pet/state", serde_json::json!({ "state": "eat" }));
    let _ = app.emit("pet/bubble", serde_json::json!({ "text": named(text, &settings), "ms": 4000 }));
    rebuild_tray_later(app);
}

// ---------- Tauri 命令 ----------

/// 亲密度称号（只增不减，无衰减惩罚；阈值见 docs/interaction-spec.md）
fn bond_title(bond: i64) -> &'static str {
    match bond {
        b if b < 5 => "初识",
        b if b < 20 => "熟悉",
        b if b < 50 => "亲密",
        _ => "挚友",
    }
}

/// 专注中被点击一次（开小差）：前端上报，用于结算卡展示
#[tauri::command]
fn note_distraction(state: tauri::State<AppState>) {
    state.distractions.fetch_add(1, Ordering::SeqCst);
}


#[tauri::command]
fn get_settings(state: tauri::State<AppState>) -> Settings {
    state.settings.lock().unwrap().clone()
}

#[tauri::command]
fn get_current_pack(state: tauri::State<AppState>) -> String {
    state.settings.lock().unwrap().pet_pack.clone()
}

#[tauri::command]
fn set_settings(app: AppHandle, state: tauri::State<AppState>, patch: Value) -> Result<(), String> {
    let mut s = state.settings.lock().unwrap();
    let mut pet_pack_changed = false;
    let mut name_changed = false;

    if let Some(v) = patch.get("focus_min").and_then(|x| x.as_i64()) {
        s.focus_min = v.clamp(1, 360); // 防御性钳制，防任意调用方写入异常值
        s.save_key(&state.db, "focus_min");
    }
    if let Some(v) = patch.get("short_min").and_then(|x| x.as_i64()) { s.short_min = v; s.save_key(&state.db, "short_min"); }
    if let Some(v) = patch.get("long_every").and_then(|x| x.as_i64()) { s.long_every = v; s.save_key(&state.db, "long_every"); }
    if let Some(v) = patch.get("long_min").and_then(|x| x.as_i64()) { s.long_min = v; s.save_key(&state.db, "long_min"); }
    if let Some(v) = patch.get("pet_pack").and_then(|x| x.as_str()) {
        if s.pet_pack != v {
            s.pet_pack = v.into();
            s.save_key(&state.db, "pet_pack");
            pet_pack_changed = true;
        }
    }
    if let Some(v) = patch.get("click_through").and_then(|x| x.as_bool()) {
        s.click_through = v;
        s.save_key(&state.db, "click_through");
        if let Some(w) = pet_window(&app) {
            let _ = w.set_ignore_cursor_events(v);
        }
    }
    if let Some(v) = patch.get("hide_bubble").and_then(|x| x.as_bool()) {
        s.hide_bubble = v;
        s.save_key(&state.db, "hide_bubble");
    }
    if let Some(v) = patch.get("pet_name").and_then(|x| x.as_str()) {
        // 命名规则（interaction-spec §3）：trim 后 ≤12 字符，空 = 回到形象默认名
        let trimmed: String = v.trim().chars().take(12).collect();
        if s.pet_name != trimmed {
            s.pet_name = trimmed;
            s.save_key(&state.db, "pet_name");
            name_changed = true;
        }
    }
    if let Some(v) = patch.get("autostart").and_then(|x| x.as_bool()) {
        s.autostart = v;
        s.save_key(&state.db, "autostart");
        use tauri_plugin_autostart::ManagerExt as _;
        let autostart = app.autolaunch();
        let _ = if v { autostart.enable() } else { autostart.disable() };
    }
    drop(s);
    if pet_pack_changed {
        let pack = state.settings.lock().unwrap().pet_pack.clone();
        let _ = app.emit("pet/set-pack", serde_json::json!({ "pack": pack }));
        rebuild_tray_later(&app);
    }
    if name_changed {
        let name = {
            let st = state.settings.lock().unwrap();
            pet_display_name(&st)
        };
        let _ = app.emit("pet/name", serde_json::json!({ "name": name }));
        rebuild_tray_later(&app); // tooltip 带 name
    }
    Ok(())
}

#[tauri::command]
fn get_pet_name(state: tauri::State<AppState>) -> String {
    let s = state.settings.lock().unwrap();
    pet_display_name(&s)
}

#[tauri::command]
fn pause_resume(state: tauri::State<AppState>) {
    let _ = state.timer.0.send(timer::Cmd::PauseResume);
}

#[tauri::command]
fn alert_rest(app: AppHandle, state: tauri::State<AppState>) {
    let s = state.settings.lock().unwrap().clone();
    let _ = state.timer.0.send(timer::Cmd::StartRest {
        short_min: s.short_min as u64,
        long_min: s.long_min as u64,
        long_every: s.long_every as u64,
    });
    // 完成一轮专注 → 零食 +1（上限 9，interaction-spec §2）
    {
        let mut st = state.settings.lock().unwrap();
        st.snacks = (st.snacks + 1).min(9);
        st.save_key(&state.db, "snacks");
    }
    hide_alert_ui(&app, "rest"); // 前端：happy 播一遍落定到 sit 循环
    let _ = app.emit("pet/bubble", serde_json::json!({ "text": named("好耶！去休息吧，{n}也睡会儿～", &s), "ms": 4000 }));

    // 结算卡（P0-1）：完成瞬间的正反馈 = 本轮时长 + 今日累计 + 亲密度
    // 引擎把本轮落库是异步的，稍等再查，避免漏掉刚结束的一轮
    let app2 = app.clone();
    let round_min = s.focus_min;
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(400));
        let state = app2.state::<AppState>();
        let (today_rounds, today_min) = state.db.stats_today();
        let st = state.settings.lock().unwrap().clone();
        // 本轮开小差次数：读后清零（下一轮从 0 开始）
        let distractions = state.distractions.swap(0, Ordering::SeqCst);
        let _ = app2.emit(
            "stats/settle",
            serde_json::json!({
                "round_min": round_min,
                "today_rounds": today_rounds,
                "today_min": today_min,
                "snacks": st.snacks,
                "bond": st.bond,
                "bond_title": bond_title(st.bond),
                "distractions": distractions,
            }),
        );
    });
}

#[tauri::command]
fn alert_snooze(app: AppHandle, state: tauri::State<AppState>) {
    let _ = state.timer.0.send(timer::Cmd::SnoozeFocus { min: 5 });
    hide_alert_ui(&app, "walk");
    let s = state.settings.lock().unwrap().clone();
    let _ = app.emit("pet/bubble", serde_json::json!({ "text": named("那就…再等你 5 分钟哦", &s), "ms": 4000 }));
}

#[tauri::command]
fn alert_skip(app: AppHandle, state: tauri::State<AppState>) {
    let _ = state.timer.0.send(timer::Cmd::Skip);
    hide_alert_ui(&app, "sad");
}

#[tauri::command]
fn alert_timeout(app: AppHandle, state: tauri::State<AppState>) {
    // 60s 无响应：引擎按 PRD 记「忽略」（区别于用户主动「跳过」），回到空闲
    let _ = state.timer.0.send(timer::Cmd::AlertTimeout);
    let s = state.settings.lock().unwrap().clone();
    // 无响应是「被辜负」的时刻：短暂 sad（P0-7 补全 sad 触发路径）
    hide_alert_ui(&app, "sad");
    let _ = app.emit("pet/bubble", serde_json::json!({ "text": named("{n}先回窝了哦，需要就叫我。", &s), "ms": 3500 }));
}

#[tauri::command]
fn rest_done(app: AppHandle, state: tauri::State<AppState>) {
    let min = state.settings.lock().unwrap().focus_min as u64;
    let _ = state.timer.0.send(timer::Cmd::RestDone);
    let _ = state.timer.0.send(timer::Cmd::StartFocus { min });
    hide_alert_ui(&app, "focus");
}

#[tauri::command]
fn rest_snooze(app: AppHandle, state: tauri::State<AppState>) {
    let _ = state.timer.0.send(timer::Cmd::RestSnooze { min: 5 });
    hide_alert_ui(&app, "walk");
}

fn build_context_menu(
    app: &AppHandle,
    s: &Settings,
    phase: &str,
    paused: bool,
    remain: u64,
) -> tauri::Result<tauri::menu::Menu<Wry>> {
    let running = matches!(phase, "focus" | "rest");
    let alerting = matches!(phase, "alert-focus" | "alert-rest");

    let status = tauri::menu::MenuItemBuilder::with_id("status", phase_label(phase, paused, remain))
        .enabled(false)
        .build(app)?;
    // 今日战绩行（不可点，纯信息）：与托盘、结算卡同源
    let (today_rounds, today_min) = app.state::<AppState>().db.stats_today();
    let stats_line = tauri::menu::MenuItemBuilder::with_id(
        "stats_today",
        format!("今日 🍅 ×{} · 专注 {} 分钟", today_rounds, today_min),
    )
    .enabled(false)
    .build(app)?;
    let sep1 = tauri::menu::PredefinedMenuItem::separator(app)?;
    let sep2 = tauri::menu::PredefinedMenuItem::separator(app)?;
    let sep3 = tauri::menu::PredefinedMenuItem::separator(app)?;

    let start = tauri::menu::MenuItemBuilder::with_id("start", "开始专注").build(app)?;
    let toggle = tauri::menu::MenuItemBuilder::with_id(
        "pause_resume",
        if paused {
            if phase == "rest" { "继续休息" } else { "继续专注" }
        } else if phase == "rest" {
            "暂停休息"
        } else {
            "暂停一下"
        },
    )
    .build(app)?;
    let skip = tauri::menu::MenuItemBuilder::with_id(
        "skip",
        if phase == "rest" { "跳过这次休息" } else { "跳过这次专注" },
    )
    .build(app)?;
    let feed = tauri::menu::MenuItemBuilder::with_id(
        "feed",
        format!("投喂 · 小鱼干 ×{}", s.snacks),
    )
    .enabled(s.snacks > 0)
    .build(app)?;
    let settings_item = tauri::menu::MenuItemBuilder::with_id("settings", "设置…").build(app)?;
    let hide_pet = tauri::menu::MenuItemBuilder::with_id("hide_pet", "隐藏桌宠").build(app)?;
    let quit = tauri::menu::MenuItemBuilder::with_id("quit", "退出 PetPom").build(app)?;

    let mut items: Vec<&dyn tauri::menu::IsMenuItem<Wry>> = vec![&status, &stats_line, &sep1];
    if running {
        items.push(&toggle);
        items.push(&skip);
    } else if !alerting {
        items.push(&start);
    }
    if !alerting {
        items.push(&feed);
    }
    items.push(&sep2);
    items.push(&settings_item);
    items.push(&hide_pet);
    items.push(&sep3);
    items.push(&quit);
    tauri::menu::MenuBuilder::new(app).items(&items).build()
}

#[tauri::command]
fn show_context_menu(app: AppHandle) {
    let (s, phase, paused, remain) = {
        let state = app.state::<AppState>();
        let ui = state.ui_phase.lock().unwrap().clone();
        let s = state.settings.lock().unwrap().clone();
        (s, ui.0, ui.1, ui.3)
    };
    let a = app.clone();
    let _ = app.run_on_main_thread(move || {
        match build_context_menu(&a, &s, &phase, paused, remain) {
            Ok(menu) => {
                if let Some(w) = pet_window(&a) {
                    let _ = w.popup_menu(&menu);
                }
            }
            Err(e) => eprintln!("[petpom] context menu build failed: {e}"),
        }
    });
}

#[tauri::command]
fn frontend_ready(info: String) {
    eprintln!("[frontend] ready: {}", info);
}

#[tauri::command]
fn frontend_error(msg: String) {
    eprintln!("[frontend] ERROR: {}", msg);
}

// ---------- 入口 ----------

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        // 全局唯一菜单事件分发：托盘菜单 + 宠物右键菜单共用（见 on_menu_action 注释）
        .on_menu_event(|app, event| on_menu_action(app, event.id().as_ref()))
        .setup(|app| {
            let handle = app.handle().clone();
            // 数据目录：默认 ~/Library/Application Support/com.dashu.petpom；
            // 开发/调试可用 PETPOM_DATA_DIR 覆盖（如指向项目内 data/，便于清理）
            let data_dir = match std::env::var("PETPOM_DATA_DIR") {
                Ok(d) if !d.is_empty() => std::path::PathBuf::from(d),
                _ => handle.path().app_data_dir()?,
            };
            std::fs::create_dir_all(&data_dir)?;
            let db = Db::open(&data_dir.join("petpom.db"))?;
            let settings = Settings::load(&db);

            // 宠物窗口初始位置：右下角（或恢复上次位置）
            if let Some(w) = handle.get_webview_window("pet") {
                let (mut x, mut y) = (settings.pet_x, settings.pet_y);
                // 屏幕外脏数据自愈：位置不在任何显示器逻辑范围内则回右下角
                if x >= 0.0 && y >= 0.0 {
                    let visible = handle.available_monitors().map(|ms| {
                        ms.iter().any(|m| {
                            let sc = m.scale_factor();
                            let mp = m.position();
                            let msz = m.size();
                            let x0 = mp.x as f64 / sc;
                            let y0 = mp.y as f64 / sc;
                            let x1 = x0 + msz.width as f64 / sc;
                            let y1 = y0 + msz.height as f64 / sc;
                            x >= x0 - 50.0 && x <= x1 && y >= y0 - 50.0 && y <= y1
                        })
                    }).unwrap_or(true);
                    if !visible {
                        eprintln!("[petpom] saved position ({x},{y}) off-screen, resetting");
                        x = -1.0;
                        y = -1.0;
                    }
                }
                if x < 0.0 || y < 0.0 {
                    if let Some(m) = w.current_monitor().ok().flatten() {
                        let ms = m.size();
                        let scale = m.scale_factor();
                        let lw = ms.width as f64 / scale;
                        let lh = ms.height as f64 / scale;
                        x = lw - PET_SIZE.0 - 24.0;
                        y = lh - PET_SIZE.1 - 24.0;
                    }
                }
                let _ = w.set_position(LogicalPosition::new(x, y));
                let _ = w.set_ignore_cursor_events(settings.click_through);
                {
                    let mut s = settings.clone();
                    s.pet_x = x;
                    s.pet_y = y;
                    s.save_key(&db, "pet_x");
                    s.save_key(&db, "pet_y");
                }
            }

            // 番茄钟引擎
            let h2 = handle.clone();
            let db2_data = data_dir.clone();
            let record_db = std::sync::Arc::new(Mutex::new(
                Db::open(&db2_data.join("petpom.db")).expect("second db open"),
            ));
            let record = Box::new(move |min: u64, snoozes: u64, result: &str, started: Option<std::time::SystemTime>| {
                let start_unix = started
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                record_db.lock().unwrap().insert_round(start_unix, min as i64, snoozes as i64, result);
            });
            let timer = timer::spawn(
                handle.clone(),
                record,
                Box::new(|app, limited| show_alert_ui(app, "focus-end", limited)),
                Box::new(|app| show_alert_ui(app, "rest-end", false)),
                Box::new(|app| {
                    let _ = app.emit("pet/toast", serde_json::json!({ "text": "离开太久了，这一轮就当休息过啦。重新开始一轮吧！" }));
                }),
                {
                    let app = handle.clone();
                    Box::new(move |phase: &str, remain: u64, paused: bool| {
                        // 托盘 UI 同步：标题每秒刷新；phase/paused/紧急阈值变化时换图标+重建菜单
                        let Some(state) = app.try_state::<AppState>() else { return };
                        let urgent = phase == "focus" && !paused && remain <= 60;
                        let (changed, prev_phase) = {
                            let mut cur = state.ui_phase.lock().unwrap();
                            let changed = cur.0 != phase || cur.1 != paused || cur.2 != urgent;
                            let prev = cur.0.clone();
                            *cur = (phase.to_string(), paused, urgent, remain);
                            (changed, prev)
                        };
                        if let Some(item) = state.tray_status.lock().unwrap().as_ref() {
                            let _ = item.set_text(phase_label(phase, paused, remain));
                        }
                        if changed {
                            rebuild_tray_later(&app);
                        }

                        // ---- 专注反应（interaction-spec #3/#4，每轮各 1 次）----
                        if phase == "focus" && !paused {
                            if prev_phase != "focus" {
                                state.react_half.store(false, Ordering::SeqCst);
                                state.react_final.store(false, Ordering::SeqCst);
                            }
                            let settings = state.settings.lock().unwrap().clone();
                            let bubble_on = !settings.hide_bubble; // 专注时隐藏气泡设置同样作用于陪跑文案
                            let total = settings.focus_min.max(1) as u64 * 60;
                            // 过半：只出陪跑文案，不插播 micro-* 动画
                            // （micro 素材无书本道具，插播会让"书突然消失"；专注期保持看书连续）
                            if !state.react_half.load(Ordering::SeqCst)
                                && remain > 60
                                && total > 0
                                && remain * 2 <= total
                            {
                                state.react_half.store(true, Ordering::SeqCst);
                                if bubble_on {
                                    let text = pick(&[
                                        "过半啦！{n}陪你一起撑住～",
                                        "上半场很稳，{n}偷偷给你点赞。",
                                        "节奏很好，别急，慢慢来。",
                                    ]);
                                    let _ = app.emit("pet/bubble", serde_json::json!({ "text": named(text, &settings), "ms": 3500 }));
                                }
                            }
                            // 最后 60 秒：只出陪跑打气文案（与 urgent 图标同阈值），理由同上
                            if !state.react_final.load(Ordering::SeqCst) && remain <= 60 && remain > 0 {
                                state.react_final.store(true, Ordering::SeqCst);
                                if bubble_on {
                                    let text = pick(&[
                                        "最后 1 分钟！{n}把耳朵竖起来等你～",
                                        "冲刺啦，马上就到，别停！",
                                        "马上就到，{n}好激动！",
                                    ]);
                                    let _ = app.emit("pet/bubble", serde_json::json!({ "text": named(text, &settings), "ms": 3500 }));
                                }
                            }
                        }
                    })
                },
            );

            app.manage(AppState {
                db,
                settings: Mutex::new(settings),
                timer,
                alerting: AtomicBool::new(false),
                last_saved_pos: Mutex::new((0.0, 0.0)),
                tray_status: Mutex::new(None),
                ui_phase: Mutex::new(("idle".into(), false, false, 0)),
                hidden: AtomicBool::new(false),
                react_half: AtomicBool::new(false),
                react_final: AtomicBool::new(false),
                distractions: std::sync::atomic::AtomicU64::new(0),
            });

            build_tray(&h2)?;

            // 窗口拖动后持久化位置（阈值 8px 防抖）——挂在 pet 窗口自身
            let h3 = handle.clone();
            if let Some(w) = h3.get_webview_window("pet") {
                let wf = w.clone();
                w.on_window_event(move |event| {
                    if let tauri::WindowEvent::Moved(pos) = event {
                        let state = h3.state::<AppState>();
                        if state.alerting.load(Ordering::SeqCst) {
                            return; // 提醒态窗口位移不记录
                        }
                        // Moved 上报的是物理像素，必须先转逻辑坐标再存，
                        // 否则 Retina 下每轮翻倍，位置指数级漂移出屏
                        let scale = wf.scale_factor().unwrap_or(1.0);
                        let logical = pos.to_logical::<f64>(scale);
                        let mut last = state.last_saved_pos.lock().unwrap();
                        let (lx, ly) = (logical.x, logical.y);
                        if (lx - last.0).abs() < 8.0 && (ly - last.1).abs() < 8.0 {
                            return;
                        }
                        *last = (lx, ly);
                        let mut s = state.settings.lock().unwrap();
                        s.pet_x = lx;
                        s.pet_y = ly;
                        s.save_key(&state.db, "pet_x");
                        s.save_key(&state.db, "pet_y");
                    }
                });
            }

            // 设置窗口不再静态声明：改为首次打开时懒创建（见 open_settings），
            // 省去常驻 WebView 约 38MB（内存红线复核 2026-09-15 结论）。

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_settings,
            note_distraction,
            get_current_pack,
            set_settings,
            drag_move,
            pause_resume,
            alert_rest,
            alert_snooze,
            alert_skip,
            alert_timeout,
            rest_done,
            rest_snooze,
            show_context_menu,
            get_pet_name,
            frontend_ready,
            frontend_error
        ])
        .run(tauri::generate_context!())
        .expect("PetPom 启动失败");
}
