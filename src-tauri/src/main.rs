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
use tauri::{PhysicalPosition, AppHandle, Emitter, LogicalSize, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder, Wry};

// 2026-09-30：窗口从 315 压到 235（气泡/时间牌下移贴身，顶部空白不再浪费）
const PET_SIZE: (f64, f64) = (255.0, 235.0);
const ALERT_SIZE: (f64, f64) = (765.0, 420.0);
/// 宠物在窗口里的定位（与 index.html 的 #pet 一致）：左内边距、底内边距、画布边长。
const PET_LEFT_CSS: f64 = 18.0;
const PET_BOTTOM_CSS: f64 = 21.0;
const PET_BOX_CSS: f64 = 192.0;

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
    /// 窗口位置：**物理像素**（全局坐标系，可有负值——副屏在主屏左侧/上方时为负）。
    /// 2026-09-30 修复：此前存逻辑像素（按窗口所在屏的 scale 折算），多显示器下
    /// 同一物理点在不同屏会算出不同的逻辑值 → 拖动跨屏失败、恢复位置错乱。
    pet_x: f64,
    pet_y: f64,
    /// 最近一次「每日首次」送鱼日期（YYYY-MM-DD）。只做惊喜，不做断签惩罚：
    /// 漏一天就是少一条鱼，没有连续奖励、没有补签、没有损失。
    last_gift_date: String,
    /// 窗口几何版本：窗口尺寸变化时用一次性迁移（见 startup 迁移块）
    geom_v: String,
    /// 探险开关（2026-10-01）：关掉后投喂只给解释，不消耗鱼——把「被它绑住」的隐忧交还给用户
    expedition: bool,
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
            last_gift_date: String::new(),
            geom_v: String::new(),
            expedition: true,
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
        // 2026-09-30 起坐标按「物理像素」存储（见 pet_xp 注释）；旧的逻辑坐标键不再读取
        from_db("pet_xp", &mut |v| s.pet_x = v.parse().unwrap_or(s.pet_x));
        from_db("pet_yp", &mut |v| s.pet_y = v.parse().unwrap_or(s.pet_y));
        from_db("last_gift_date", &mut |v| s.last_gift_date = v.to_string());
        from_db("geom_v", &mut |v| s.geom_v = v.to_string());
        from_db("expedition", &mut |v| s.expedition = v == "true");
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
            "pet_xp" => self.pet_x.to_string(),
            "pet_yp" => self.pet_y.to_string(),
            "last_gift_date" => self.last_gift_date.clone(),
            "geom_v" => self.geom_v.clone(),
            "expedition" => self.expedition.to_string(),
            // 2026-09-30：未知键必须留痕。此前是静默 return——改坐标存储键时漏改两处调用，
            // 位置根本没落库却毫无报错（排查成本极高）。
            other => {
                eprintln!("[petpom] save_key 未知键，已忽略: {other}");
                return;
            }
        };
        db.set_setting(key, &v);
    }
}

/// 探险看门狗：到点就把卡片带回来（app 没开时的时间差在启动补结算里处理）
fn spawn_expedition_watcher(app: AppHandle) {
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_secs(20));
        settle_due_expeditions(&app, false);
    });
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
    /// 是否正在拖拽（用于「拖拽中到点提醒延后到松手」）
    dragging: AtomicBool,
    /// 拖拽期间到点的提醒，等松手后补发
    pending_alert: Mutex<Option<(String, bool)>>,
    /// 拖拽期间的权威位置（物理像素）。拖动只累加这里，不每帧回读窗口位置——
    /// IPC + set_position 是异步的，回读到的是上一帧旧值，会把位移吃掉（表现为拖不动/卡住）
    drag_pos: Mutex<Option<(f64, f64)>>,
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
/// 起拖：读一次真实窗口位置作为累加基准（只在拖拽开始时读，之后不再回读）
#[tauri::command]
fn drag_begin(app: AppHandle, state: tauri::State<AppState>) {
    state.dragging.store(true, Ordering::SeqCst);
    if let Some(w) = pet_window(&app) {
        if let Ok(p) = w.outer_position() {
            *state.drag_pos.lock().unwrap() = Some((p.x as f64, p.y as f64));
        }
    }
}

/// 结束拖拽：清理累加器（位置由 window Moved 事件落库）
#[tauri::command]
fn drag_end(app: AppHandle, state: tauri::State<AppState>) {
    *state.drag_pos.lock().unwrap() = None;
    state.dragging.store(false, Ordering::SeqCst);
    // 拖拽期间到点的提醒：松手后立刻补发（视觉上「拖完再叫」）
    let pending = state.pending_alert.lock().unwrap().take();
    if let Some((kind, limited)) = pending {
        show_alert_ui(&app, &kind, limited);
    }
}

/// 增量拖动：按**物理像素**增量移动窗口。
/// 为什么不用绝对坐标：LogicalPosition 会按窗口当前所在的屏折算 scale，
/// 跨屏时 scale 变化会让同一个逻辑坐标指向不同物理点 → 表现为拖到副屏就“卡住/乱跳”。
/// 增量 + 物理坐标全程不涉及 scale，天然多屏安全。
#[tauri::command]
fn drag_move_by(app: AppHandle, state: tauri::State<AppState>, dx: f64, dy: f64) {
    let mut g = state.drag_pos.lock().unwrap();
    let Some((x, y)) = *g else { return }; // 未 begin：忽略（前端会补 begin）
    let (nx, ny) = (x + dx, y + dy);
    *g = Some((nx, ny));
    if let Some(w) = pet_window(&app) {
        let _ = w.set_position(PhysicalPosition::new(nx, ny));
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
    // 手指还按着时不抢：记下来，等 drag_end 补发（避免拖到一半窗口突变、宠物被甩开）
    if state.dragging.load(Ordering::SeqCst) {
        *state.pending_alert.lock().unwrap() = Some((kind.to_string(), limited));
        return;
    }
    let settings = state.settings.lock().unwrap().clone();
    // 提醒优先（interaction-spec 红线「提醒到点强制弹现」）：
    // 它若正在外面，必须赶回来——否则窗口弹了、宠物却还隐身着，提醒变成没有主角。
    // 行程保留、卡片照给（守住专注是它的职责，优先级高于这趟探索）。
    {
        let stamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        if let Some(key) = state.db.force_return_active(&stamp) {
            if let Some(card) = card_by_key(&key) {
                let _ = app.emit(
                    "pet/card",
                    serde_json::json!({
                        "key": card.key, "name": card.name, "text": card.text,
                        "rarity": card.rarity, "rarity_label": RARITY_LABEL[card.rarity as usize],
                        "startup": false, "rushed": true,
                    }),
                );
            }
        }
    }
    if let Some(w) = pet_window(app) {
        let _ = w.show(); // 隐藏态也要弹现提醒（interaction-spec 红线）
        let _ = w.set_size(LogicalSize::new(ALERT_SIZE.0, ALERT_SIZE.1));
        // 左扩 + 上移：尺寸/位移是逻辑值，窗口坐标是物理值 → 乘 scale 换算
        // 提醒态窗口锚点：让「宠物」在两套窗口尺寸下屏幕位置不变。
        // 水平：放宽后宠物画在窗口右侧（index.html 的 #stage.alerting #pet），
        //       所以左移 PET_SIZE+PET_LEFT*2 的差值；垂直：底边对齐由高度差决定。
        let sc = w.scale_factor().unwrap_or(1.0);
        let alert_dx = PET_LEFT_CSS - (ALERT_SIZE.0 - PET_LEFT_CSS - PET_BOX_CSS);
        let pet_top_in_pet_win = PET_SIZE.1 - PET_BOTTOM_CSS - PET_BOX_CSS;
        let pet_top_in_alert_win = ALERT_SIZE.1 - PET_BOTTOM_CSS - PET_BOX_CSS;
        let alert_dy = pet_top_in_pet_win - pet_top_in_alert_win;
        let _ = w.set_position(PhysicalPosition::new(
            settings.pet_x + alert_dx * sc,
            settings.pet_y + alert_dy * sc,
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
        let _ = w.set_position(PhysicalPosition::new(settings.pet_x, settings.pet_y));
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

// ---------- 它的小窝：探险带回的卡片池（2026-09-23） ----------
// 调性：温暖、克制、不夸张。卡片 = 它出门路上捡到的小东西 + 一句它想说的话。
// rarity: 0 普通 / 1 少见 / 2 稀有
struct Card {
    key: &'static str,
    name: &'static str,
    text: &'static str,
    rarity: u8,
}

const CARDS: &[Card] = &[
    // —— 普通 ——
    Card { key: "pebble", name: "海边的小石头", text: "被水磨得很圆，握在手里温温的。", rarity: 0 },
    Card { key: "leaf", name: "一片银杏叶", text: "黄得刚刚好，我夹在书里带回来了。", rarity: 0 },
    Card { key: "cocoa", name: "热可可的纸杯", text: "喝完了，但杯壁还留着一点甜味。", rarity: 0 },
    Card { key: "feather", name: "一根羽毛", text: "不知道是谁掉的，很轻，放在窗台会飘。", rarity: 0 },
    Card { key: "ticket", name: "半张旧车票", text: "上面的字都褪了，只看得清一个日期。", rarity: 0 },
    Card { key: "flower", name: "一朵小野花", text: "长在路边石缝里，我小心地摘了。", rarity: 0 },
    Card { key: "chestnut", name: "一颗栗子", text: "掉在地上，还带着壳的刺。", rarity: 0 },
    Card { key: "sunlight", name: "窗台上的阳光", text: "我趴了一会儿，暖的。分你一半。", rarity: 0 },
    Card { key: "cloud", name: "云的影子", text: "从屋顶上慢慢走过去，我数了七秒。", rarity: 0 },
    Card { key: "notecard", name: "它的便签", text: "写着：今天也要记得喝水。", rarity: 0 },
    Card { key: "no-worry", name: "夹在书里的小纸条", text: "不用天天喂我。你专心的时候，我自己找得到事做。", rarity: 0 },
    // —— 少见 ——
    Card { key: "wish", name: "陌生人的心愿便签", text: "贴在路灯上，写着「希望妈妈早点好起来」。", rarity: 1 },
    Card { key: "key", name: "生锈的小钥匙", text: "不知道开哪扇门，我先收好了。", rarity: 1 },
    Card { key: "ladybug", name: "睡着的瓢虫", text: "在叶子背面，我看了很久没舍得弄醒它。", rarity: 1 },
    Card { key: "marble", name: "玻璃弹珠", text: "对着光看，里面有彩色的细线。", rarity: 1 },
    Card { key: "recipe", name: "手写的食谱", text: "字很好看，只抄到一半：「小火，慢慢来」。", rarity: 1 },
    // —— 稀有 ——
    Card { key: "clover", name: "四叶草", text: "蹲下来找了很久。这个给你，会好运的。", rarity: 2 },
    Card { key: "seaglass", name: "海玻璃", text: "在河边捡的，被水磨了不知道多少年。", rarity: 2 },
    Card { key: "star", name: "夜里的一颗星", text: "我盯了很久，好像真的摘到了一颗。", rarity: 2 },
    Card { key: "coin", name: "一枚旧硬币", text: "年份比我大得多，边缘被摸得发亮。", rarity: 2 },
    Card { key: "rainbow", name: "一小片彩虹", text: "雨后出现在水洼里，只停了两分钟。", rarity: 2 },
];

fn card_by_key(key: &str) -> Option<&'static Card> {
    CARDS.iter().find(|c| c.key == key)
}

const RARITY_LABEL: [&str; 3] = ["寻常", "少见", "珍贵"];

/// 抽卡：先按稀有度掷点，再在该档里**优先给没收集过的**。
/// 不做「全池纯随机」的原因：那样集齐要 60+ 次投喂（优惠券收集问题），
/// 用户会觉得「一直在拿重复的」，而重复卡带不来任何新东西。
fn roll_card(owned: &std::collections::HashSet<String>) -> &'static Card {
    let n = nanos();
    let roll = (n / 7) % 100; // 与探险时长用的随机位错开
    let rarity: u8 = if roll < 10 { 2 } else if roll < 40 { 1 } else { 0 };
    let node: Vec<&Card> = CARDS.iter().filter(|c| c.rarity == rarity).collect();
    let unowned: Vec<&Card> = node.iter().copied().filter(|c| !owned.contains(c.key)).collect();
    let pool = if unowned.is_empty() { node } else { unowned };
    pool[(n as usize / 13) % pool.len()]
}

fn nanos() -> u32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0)
}

/// 气泡（尊重「专注时隐藏气泡」设置）
fn say(app: &AppHandle, text: &str, ms: u64) {
    let state = app.state::<AppState>();
    if state.settings.lock().unwrap().hide_bubble {
        return;
    }
    let _ = app.emit("pet/bubble", serde_json::json!({ "text": text, "ms": ms }));
}

/// 探险到点结算：把卡片收进小窝并通知前端。
/// startup=true 表示这是 app 启动时的补结算（离线产出：app 没开也在走时间）
fn settle_due_expeditions(app: &AppHandle, startup: bool) {
    let state = app.state::<AppState>();
    let now = chrono::Local::now();
    let due = state.db.due_expeditions(now.timestamp());
    if due.is_empty() {
        return; // 无变化不重建托盘（看门线程每 20s 跑一次，否则菜单会一直闪）
    }
    for (id, key) in due {
        let stamp = now.format("%Y-%m-%d %H:%M:%S").to_string();
        state.db.return_expedition(id, &key, &stamp);
        if let Some(card) = card_by_key(&key) {
            let _ = app.emit(
                "pet/card",
                serde_json::json!({
                    "key": card.key, "name": card.name, "text": card.text,
                    "rarity": card.rarity, "rarity_label": RARITY_LABEL[card.rarity as usize],
                    "startup": startup,
                }),
            );
        }
    }
    rebuild_tray_later(app);
}

/// 撤销式召回：把出门的宠物叫回来（2026-09-23 队长决定）。
/// 这趟作废——退还小鱼干、回退亲密度、没有卡片。
/// 敢给这个出口的原因：提前召回拿不到卡片，所以没人会用它绕过等待，
/// 「有出口」和「有时间门槛」可以同时成立。
fn recall_expedition_internal(app: &AppHandle) {
    let state = app.state::<AppState>();
    if state.db.cancel_active_expedition().is_none() {
        say(app, "它就在家呀～", 3000);
        return;
    }
    {
        let mut s = state.settings.lock().unwrap();
        s.snacks = (s.snacks + 1).min(9);
        s.bond = (s.bond - 1).max(0);
        s.save_key(&state.db, "snacks");
        s.save_key(&state.db, "bond");
    }
    let s = state.settings.lock().unwrap().clone();
    let _ = app.emit("pet/recall", serde_json::json!({}));
    say(app, &named("我先回来啦…这趟什么都没找到，小鱼干还你～", &s), 4500);
    rebuild_tray_later(app);
}

/// 每天第一次打开时送一条小鱼干。纯惊喜，不做连续奖励、不惩罚漏签。
fn daily_first_gift(app: &AppHandle) {
    let state = app.state::<AppState>();
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    {
        let mut s = state.settings.lock().unwrap();
        if s.last_gift_date == today {
            return;
        }
        s.last_gift_date = today;
        s.snacks = (s.snacks + 1).min(9);
        s.save_key(&state.db, "last_gift_date");
        s.save_key(&state.db, "snacks");
    }
    let s = state.settings.lock().unwrap().clone();
    let _ = app.emit("pet/state", serde_json::json!({ "state": "happy" }));
    say(app, &named("今天的第一条小鱼干，{n}给你带了一条～", &s), 4500);
    rebuild_tray_later(app);
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

    // 投喂项：与右键菜单同源。2026-09-23 起不再灰掉——没鱼/已出门时点它都会得到明确解释
    let trip_left = state
        .db
        .active_expedition()
        .filter(|(_, due, _, _)| *due > chrono::Local::now().timestamp())
        .map(|(_, due, _, _)| ((due - chrono::Local::now().timestamp() + 59) / 60).max(1));
    let feed_label = match trip_left {
        Some(m) => format!("它出门中 · 约 {} 分钟后回来", m),
        None => format!("投喂小鱼干 · 还有 ×{}", s.snacks),
    };
    let feed = tauri::menu::MenuItemBuilder::with_id("feed", feed_label).build(app)?;
    let recall = tauri::menu::MenuItemBuilder::with_id("recall", "叫它回来")
        .enabled(trip_left.is_some())
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
    items.push(&recall);
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
        "recall" => recall_expedition_internal(app),
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
/// 投喂小鱼干 = 送它出门探险（2026-09-23 改版）
/// 机制取向「温和期待型」：回来一定会带东西，带什么不确定；
/// 不惩罚缺席、不衰减、不索要——只为「想知道它带回了什么」而回来。
fn feed_pet_internal(app: &AppHandle) {
    let state = app.state::<AppState>();
    let now = chrono::Local::now();

    // 1) 还在外面：给明确反馈（旧版是灰掉菜单项，用户不知道发生了什么）
    if let Some((_, due, _, _)) = state.db.active_expedition() {
        if due > now.timestamp() {
            let left = ((due - now.timestamp()) as f64 / 60.0).ceil() as i64;
            say(app, &format!("它还在外面转悠呢，大概 {} 分钟后回来～", left.max(1)), 4000);
            return;
        }
    }

    // 2) 探险被关掉：明确说明，不消耗任何东西
    if !state.settings.lock().unwrap().expedition {
        say(app, "探险先关掉啦～想在设置里重新打开就行。", 4500);
        return;
    }

    // 3) 没有鱼：说清楚鱼从哪来（这是新用户唯一能得到的引导）
    let snacks = state.settings.lock().unwrap().snacks;
    if snacks <= 0 {
        say(app, "小鱼干不够啦～ 完成一轮专注我会带一颗回来。", 4500);
        return;
    }

    // 4) 出发：扣鱼 + 亲密度 +1 + 抽定卡片 + 定到点时间
    let owned: std::collections::HashSet<String> = state
        .db
        .den_cards()
        .iter()
        .map(|(k, _, _)| k.clone())
        .collect();
    let card = roll_card(&owned);
    let mins = 12 + (nanos() % 17) as i64; // 12~28 分钟，随机但一定有结果
    let due = now + chrono::Duration::minutes(mins);
    {
        let mut s = state.settings.lock().unwrap();
        s.snacks -= 1;
        s.bond += 1; // 亲密度只增不减，无衰减惩罚
        s.save_key(&state.db, "snacks");
        s.save_key(&state.db, "bond");
    }
    state.db.start_expedition(
        &now.format("%Y-%m-%d %H:%M:%S").to_string(),
        due.timestamp(),
        card.key,
    );

    let s = state.settings.lock().unwrap().clone();
    let _ = app.emit("pet/state", serde_json::json!({ "state": "eat" }));
    say(
        app,
        &named(
            &format!("（吧唧吧唧）我出去转转，大约 {} 分钟后回来，等我带点好东西！", mins),
            &s,
        ),
        4500,
    );
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


/// 它的小窝：卡片图鉴 + 收集进度 + 进行中的探险（设置页「小窝」用）
#[tauri::command]
fn get_den(state: tauri::State<AppState>) -> serde_json::Value {
    let owned = state.db.den_cards();
    let owned_keys: std::collections::HashMap<String, (String, i64)> = owned
        .iter()
        .map(|(k, at, seen)| (k.clone(), (at.clone(), *seen)))
        .collect();
    let now = chrono::Local::now().timestamp();
    let cards: Vec<serde_json::Value> = CARDS
        .iter()
        .map(|c| {
            let got = owned_keys.get(c.key);
            serde_json::json!({
                "key": c.key, "name": c.name, "text": c.text,
                "rarity": c.rarity, "rarity_label": RARITY_LABEL[c.rarity as usize],
                "owned": got.is_some(),
                "obtained_at": got.map(|g| g.0.clone()).unwrap_or_default(),
                "seen": got.map(|g| g.1).unwrap_or(1),
            })
        })
        .collect();
    let trip = match state.db.active_expedition() {
        Some((_, due, _, started)) if due > now => serde_json::json!({
            "active": true, "remain_sec": due - now, "started_at": started
        }),
        _ => serde_json::json!({ "active": false }),
    };
    serde_json::json!({
        "total": CARDS.len(),
        "owned": owned_keys.len(),
        "unseen": state.db.unseen_count(),
        "cards": cards,
        "trip": trip,
    })
}

/// 看过小窝就把新卡片标记已读
#[tauri::command]
fn mark_den_seen(state: tauri::State<AppState>) {
    state.db.mark_den_seen();
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
    if let Some(v) = patch.get("expedition").and_then(|x| x.as_bool()) {
        s.expedition = v;
        s.save_key(&state.db, "expedition");
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
    // 未知键留痕：set_settings 是白名单式实现，新增设置项漏加分支会静默失效（踩过两次）
    if let Some(obj) = patch.as_object() {
        const KNOWN: &[&str] = &[
            "focus_min", "short_min", "long_every", "long_min", "pet_pack", "click_through",
            "hide_bubble", "expedition", "pet_name", "autostart",
        ];
        for k in obj.keys() {
            if !KNOWN.contains(&k.as_str()) {
                eprintln!("[petpom] set_settings 未知键，已忽略: {k}");
            }
        }
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
    hide_alert_ui(&app, ""); // 稳态由 phase(rest) 驱动；前端会先播 happy 庆祝再睡
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
    hide_alert_ui(&app, ""); // phase 已回到 focus → 稳态自动回到「看书」
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
    hide_alert_ui(&app, ""); // phase 变 focus → 自动回到「看书」
}

#[tauri::command]
fn rest_snooze(app: AppHandle, state: tauri::State<AppState>) {
    let _ = state.timer.0.send(timer::Cmd::RestSnooze { min: 5 });
    hide_alert_ui(&app, ""); // phase 仍为 rest → 继续睡
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
    // 投喂项：2026-09-23 起不再灰掉——没鱼/已出门时点它都会得到明确解释，
    // 灰掉只会让用户不知道发生了什么（旧版正是如此）
    let st = app.state::<AppState>();
    let trip_left = st
        .db
        .active_expedition()
        .filter(|(_, due, _, _)| *due > chrono::Local::now().timestamp())
        .map(|(_, due, _, _)| ((due - chrono::Local::now().timestamp() + 59) / 60).max(1));
    let feed_label = match trip_left {
        Some(m) => format!("它出门中 · 约 {} 分钟后回来", m),
        None => format!("投喂小鱼干 · 还有 ×{}", s.snacks),
    };
    let feed = tauri::menu::MenuItemBuilder::with_id("feed", feed_label).build(app)?;
    let recall = tauri::menu::MenuItemBuilder::with_id("recall", "叫它回来")
        .enabled(trip_left.is_some())
        .build(app)?;
    let unseen = st.db.unseen_count();
    let settings_label = if unseen > 0 {
        format!("设置…（小窝有 {} 件新东西）", unseen)
    } else {
        "设置…".to_string()
    };
    let settings_item = tauri::menu::MenuItemBuilder::with_id("settings", settings_label).build(app)?;
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
        items.push(&recall);
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
fn frontend_ready(app: AppHandle, info: String) {
    eprintln!("[frontend] ready: {}", info);
    // 前端就绪后再触发（这两件事都要弹气泡，页面没加载完会丢）
    let a = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(700));
        // 把当前阶段同步给前端：形态的唯一驱动源是 phase，页面重载后也要能还原
        {
            let st = a.state::<AppState>();
            let (phase, paused, _urgent, remain) = st.ui_phase.lock().unwrap().clone();
            let _ = a.emit(
                "timer/tick",
                serde_json::json!({ "phase": phase, "remaining_sec": remain, "paused": paused }),
            );
        }
        settle_due_expeditions(&a, true); // 离线产出：app 没开也在走时间
        daily_first_gift(&a);             // 每天第一条鱼
    });
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
                let scale = w.scale_factor().unwrap_or(1.0);
                // 一次性几何迁移：窗口高度 315 → 235 时，宠物（贴窗口底边）会上跳 80px，
                // 把 y 补回相同的差值，让宠物待在原地。用 geom_v 保证只做一次。
                {
                    let mut s = settings.clone();
                    const GEOM_V: &str = "235";
                    if s.geom_v != GEOM_V {
                        if settings.pet_x != -1.0 || settings.pet_y != -1.0 {
                            y += 315.0 - PET_SIZE.1; // 视窗变矮了多少，就往下补多少
                            eprintln!("[petpom] 几何迁移 315→235：y 下移 {:.0}px 保持宠物位置", 315.0 - PET_SIZE.1);
                        }
                        s.geom_v = GEOM_V.to_string();
                        s.save_key(&db, "geom_v");
                    }
                }
                // 屏幕外脏数据自愈：物理坐标直接和显示器的物理范围比，不做任何 scale 折算
                if x != -1.0 || y != -1.0 {
                    let visible = handle.available_monitors().map(|ms| {
                        ms.iter().any(|m| {
                            let mp = m.position();
                            let msz = m.size();
                            let x0 = mp.x as f64;
                            let y0 = mp.y as f64;
                            let x1 = x0 + msz.width as f64;
                            let y1 = y0 + msz.height as f64;
                            x >= x0 - 80.0 && x <= x1 && y >= y0 - 80.0 && y <= y1
                        })
                    }).unwrap_or(true);
                    if !visible {
                        eprintln!("[petpom] saved position ({x},{y}) off-screen, resetting");
                        x = -1.0;
                        y = -1.0;
                    }
                }
                if x == -1.0 && y == -1.0 {
                    // 默认：当前屏右下角（物理像素）
                    if let Some(m) = w.current_monitor().ok().flatten() {
                        let mp = m.position();
                        let msz = m.size();
                        x = mp.x as f64 + msz.width as f64 - (PET_SIZE.0 + 24.0) * scale;
                        y = mp.y as f64 + msz.height as f64 - (PET_SIZE.1 + 24.0) * scale;
                    }
                }
                let _ = w.set_position(PhysicalPosition::new(x, y));
                let _ = w.set_ignore_cursor_events(settings.click_through);
                {
                    let mut s = settings.clone();
                    s.pet_x = x;
                    s.pet_y = y;
                    s.save_key(&db, "pet_xp");
                    s.save_key(&db, "pet_yp");
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
            spawn_expedition_watcher(handle.clone());
            spawn_expedition_watcher(handle.clone());
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
                drag_pos: Mutex::new(None),
                dragging: AtomicBool::new(false),
                pending_alert: Mutex::new(None),
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
                        // 2026-09-30：直接存物理坐标（不再折算逻辑）——跨屏时 scale 会变，
                        // 折算出来的逻辑值不构成稳定的全局坐标系
                        let _ = &wf;
                        let mut last = state.last_saved_pos.lock().unwrap();
                        let (lx, ly) = (pos.x as f64, pos.y as f64);
                        if (lx - last.0).abs() < 8.0 && (ly - last.1).abs() < 8.0 {
                            return;
                        }
                        *last = (lx, ly);
                        let mut s = state.settings.lock().unwrap();
                        s.pet_x = lx;
                        s.pet_y = ly;
                        s.save_key(&state.db, "pet_xp");
                        s.save_key(&state.db, "pet_yp");
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
            get_den,
            mark_den_seen,
            get_current_pack,
            set_settings,
            drag_begin,
            drag_move_by,
            drag_end,
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
