//! 番茄钟引擎：独立线程 + mpsc 命令通道
//! 事件（emit 给全部窗口）——事件契约：
//!   timer/tick            {phase, remaining_sec, paused}
//!     ※ focus/rest 运行态每 500ms 持续输出，含暂停态（paused=true、
//!       remaining 冻结）——前端时间牌/托盘依赖持续 tick 渲染暂停样式，
//!       严禁改成"暂停时静默"（2026-09-03 bug 复盘）
//!   timer/focus-complete  {}   专注到点
//!   timer/rest-complete   {}   休息到点
//!   timer/invalidated     {}   休眠唤醒后本轮已失效（PRD US-7）
//!   timer/state           {phase}  idle|focus|rest|alert-focus|alert-rest（托盘刷新用）

use serde::Serialize;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant, SystemTime};
use tauri::{AppHandle, Emitter};

#[derive(Clone)]
pub struct TimerHandle(pub Sender<Cmd>);

pub enum Cmd {
    StartFocus { min: u64 },
    PauseResume,
    Skip,                      // 跳过本轮（专注/提醒/休息态均可）
    SnoozeFocus { min: u64 },  // 婉拒：N 分钟后再次提醒
    StartRest { short_min: u64, long_min: u64, long_every: u64 }, // 确认休息（引擎按轮数挑长短休）
    RestSnooze { min: u64 },   // 休息赖床
    RestDone,                  // 休息结束确认 → 开始下一轮专注
    AlertTimeout,              // 提醒 60 秒无响应 → 记「忽略」
}

#[derive(Clone, Serialize)]
struct TickPayload {
    phase: &'static str,
    remaining_sec: u64,
    paused: bool,
}

// Resting.total_min 备用于后续"休息超时"记录；AlertFocus.limited 备用于第 3 次提醒差异化文案
#[allow(dead_code)]
enum Phase {
    Idle,
    Focusing { ends_at: Instant, total_min: u64, paused_at: Option<Instant> },
    Resting { ends_at: Instant, total_min: u64, paused_at: Option<Instant> },
    /// 提醒展示中（胶囊挂起，等用户三选一 / 60s 超时）
    AlertFocus { limited: bool },
    AlertRest,
}

struct Engine {
    app: AppHandle,
    rx: Receiver<Cmd>,
    phase: Phase,
    snoozes: u64,   // 本轮婉拒次数
    rounds_done: u64, // 完成的专注轮数（决定长短休）
    last_focus_min: u64,
    last_wall: SystemTime, // 休眠检测锚点
    /// 本轮专注开始墙钟时间（落库用，2026-09-22 修复：此前 started_at=ended_at 失真）
    focus_started_at: Option<SystemTime>,
    /// 结果回调（记录 rounds），由 main 侧注入闭包
    record: Box<dyn Fn(u64, u64, &str, Option<SystemTime>) + Send>,
    /// 到点后的 UI 编排（弹胶囊/缩放窗口），由 main 侧注入
    on_focus_end: Box<dyn Fn(&AppHandle, bool) + Send + Sync>,
    on_rest_end: Box<dyn Fn(&AppHandle) + Send + Sync>,
    on_invalidated: Box<dyn Fn(&AppHandle) + Send + Sync>,
    /// UI 同步回调（托盘标题/图标/菜单重建），参数 (phase, remain_sec, paused)
    on_ui: Box<dyn Fn(&str, u64, bool) + Send + Sync>,
    last_ui: (&'static str, u64, bool), // 去重：状态未变不回调
}

const ALERT_TIMEOUT_NOTE: &str = "忽略";

impl Engine {
    fn emit_state(&self, phase: &'static str) {
        let _ = self.app.emit("timer/state", serde_json::json!({ "phase": phase }));
    }

    /// 把当前阶段/剩余秒数/暂停态推给托盘 UI（内部去重）
    fn sync_ui(&mut self) {
        let (phase, remain, paused): (&'static str, u64, bool) = match &self.phase {
            Phase::Idle => ("idle", 0, false),
            Phase::Focusing { ends_at, paused_at, .. } => {
                let r = match paused_at {
                    Some(at) => ends_at.duration_since(*at).as_secs(),
                    None => ends_at.duration_since(Instant::now()).as_secs(),
                };
                ("focus", r, paused_at.is_some())
            }
            Phase::Resting { ends_at, paused_at, .. } => {
                let r = match paused_at {
                    Some(at) => ends_at.duration_since(*at).as_secs(),
                    None => ends_at.duration_since(Instant::now()).as_secs(),
                };
                ("rest", r, paused_at.is_some())
            }
            Phase::AlertFocus { .. } => ("alert-focus", 0, false),
            Phase::AlertRest => ("alert-rest", 0, false),
        };
        if self.last_ui != (phase, remain, paused) {
            self.last_ui = (phase, remain, paused);
            (self.on_ui)(phase, remain, paused);
        }
    }

    fn tick(&mut self) {
        // ---- 休眠唤醒检测（PRD US-7）----
        let now_wall = SystemTime::now();
        if let Ok(d) = now_wall.duration_since(self.last_wall) {
            if d > Duration::from_secs(30) {
                self.handle_wake(d);
            }
        }
        self.last_wall = now_wall;

        // ---- 先判定（不可变借用），后执行（可变操作）----
        enum Fire {
            Focus,
            Rest,
        }
        let mut fire = None;
        match &self.phase {
            Phase::Focusing { ends_at, paused_at, .. } => {
                match paused_at {
                    // 暂停中也持续发 tick（paused=true）——托盘/时间牌需要
                    // 持续收到暂停态才能渲染灰色静止样式（2026-09-03 修复）
                    Some(at) => {
                        let remain = ends_at.duration_since(*at).as_secs();
                        let _ = self.app.emit(
                            "timer/tick",
                            TickPayload { phase: "focus", remaining_sec: remain, paused: true },
                        );
                    }
                    None => {
                        let now = Instant::now();
                        if now >= *ends_at {
                            fire = Some(Fire::Focus);
                        } else {
                            let remain = ends_at.duration_since(now).as_secs();
                            let _ = self.app.emit(
                                "timer/tick",
                                TickPayload { phase: "focus", remaining_sec: remain, paused: false },
                            );
                        }
                    }
                }
            }
            Phase::Resting { ends_at, paused_at, .. } => {
                match paused_at {
                    Some(at) => {
                        let remain = ends_at.duration_since(*at).as_secs();
                        let _ = self.app.emit(
                            "timer/tick",
                            TickPayload { phase: "rest", remaining_sec: remain, paused: true },
                        );
                    }
                    None => {
                        let now = Instant::now();
                        if now >= *ends_at {
                            fire = Some(Fire::Rest);
                        } else {
                            let remain = ends_at.duration_since(now).as_secs();
                            let _ = self.app.emit(
                                "timer/tick",
                                TickPayload { phase: "rest", remaining_sec: remain, paused: false },
                            );
                        }
                    }
                }
            }
            _ => {}
        }

        match fire {
            Some(Fire::Focus) => {
                let limited = self.snoozes >= 2;
                (self.on_focus_end)(&self.app, limited);
                self.emit_state("alert-focus");
                let _ = self.app.emit("timer/focus-complete", {});
                self.phase = Phase::AlertFocus { limited };
            }
            Some(Fire::Rest) => {
                (self.on_rest_end)(&self.app);
                self.emit_state("alert-rest");
                let _ = self.app.emit("timer/rest-complete", {});
                self.phase = Phase::AlertRest;
            }
            None => {}
        }

        self.sync_ui();
    }

    /// 唤醒处理：专注期超时 >10 分钟 → 本轮失效；否则把休眠时长从倒计时里扣除
    fn handle_wake(&mut self, slept: Duration) {
        if let Phase::Focusing { ends_at, total_min, paused_at: None } = self.phase {
            let now = Instant::now();
            if now > ends_at + Duration::from_secs(600) {
                (self.on_invalidated)(&self.app);
                (self.record)(total_min, self.snoozes, "休眠失效", self.focus_started_at);
                self.focus_started_at = None;
                self.phase = Phase::Idle;
                self.snoozes = 0;
                self.emit_state("idle");
                let _ = self.app.emit("timer/invalidated", {});
                return;
            }
            // 还没到点/刚超时不久：休眠时长顺延
            let new_end = ends_at + slept;
            self.phase = Phase::Focusing { ends_at: new_end, total_min, paused_at: None };
            return;
        }
        // 2026-09-23 修复：休息期同样要顺延。此前只处理 Focusing，
        // 结果「合盖睡 10 分钟 → 醒来休息已被判为结束」，凭空少一次休息
        if let Phase::Resting { ends_at, total_min, paused_at: None } = self.phase {
            self.phase = Phase::Resting {
                ends_at: ends_at + slept,
                total_min,
                paused_at: None,
            };
        }
    }

    fn handle(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::StartFocus { min } => {
                self.phase = Phase::Focusing {
                    ends_at: Instant::now() + Duration::from_secs(min * 60),
                    total_min: min,
                    paused_at: None,
                };
                self.snoozes = 0;
                self.last_focus_min = min;
                self.focus_started_at = Some(SystemTime::now());
                self.emit_state("focus");
            }
            Cmd::PauseResume => {
                // 专注/休息均可暂停（2026-09-03 修复：休息暂停此前被静默忽略）
                match &mut self.phase {
                    Phase::Focusing { ends_at, paused_at, .. } => match paused_at.take() {
                        Some(at) => {
                            *ends_at += Instant::now().duration_since(at);
                        }
                        None => *paused_at = Some(Instant::now()),
                    },
                    Phase::Resting { ends_at, paused_at, .. } => match paused_at.take() {
                        Some(at) => {
                            *ends_at += Instant::now().duration_since(at);
                        }
                        None => *paused_at = Some(Instant::now()),
                    },
                    _ => {}
                }
            }
            Cmd::Skip => {
                // 2026-09-23 修复：提醒态点「跳过本轮」此前不落库——
                // 此时 phase 已是 AlertFocus/AlertRest，旧代码只在 Focusing 才记录，
                // 导致「跳过」的轮次从今日战绩里凭空消失
                let min = match self.phase {
                    Phase::Focusing { total_min, .. } => Some(total_min),
                    Phase::AlertFocus { .. } => Some(self.last_focus_min),
                    _ => None,
                };
                if let Some(min) = min {
                    (self.record)(min, self.snoozes, "跳过", self.focus_started_at);
                    self.focus_started_at = None;
                }
                self.phase = Phase::Idle;
                self.snoozes = 0;
                self.emit_state("idle");
            }
            Cmd::SnoozeFocus { min } => {
                self.snoozes += 1;
                self.phase = Phase::Focusing {
                    ends_at: Instant::now() + Duration::from_secs(min * 60),
                    total_min: min,
                    paused_at: None,
                };
                // 2026-09-23 修复：婉拒段自成一轮，落库时长必须是这段的真实长度。
                // 旧代码把 total_min 写死 5、又不更新 last_focus_min，
                // 于是「专注 25 分钟 → 婉拒两次 → 完成」会按 25 分钟落库 → 今日累计虚高
                self.last_focus_min = min;
                self.focus_started_at = Some(SystemTime::now());
                self.emit_state("focus");
            }
            Cmd::StartRest { short_min, long_min, long_every } => {
                (self.record)(self.last_focus_min, self.snoozes, "完成", self.focus_started_at);
                self.focus_started_at = None;
                self.rounds_done += 1;
                self.snoozes = 0;
                // 每完成 long_every 轮进一次长休
                let is_long = long_every > 0 && self.rounds_done % long_every == 0;
                let min = if is_long { long_min } else { short_min };
                self.phase = Phase::Resting {
                    ends_at: Instant::now() + Duration::from_secs(min * 60),
                    total_min: min,
                    paused_at: None,
                };
                self.emit_state("rest");
            }
            Cmd::RestSnooze { min } => {
                self.phase = Phase::Resting {
                    ends_at: Instant::now() + Duration::from_secs(min * 60),
                    total_min: 5,
                    paused_at: None,
                };
                self.emit_state("rest");
            }
            Cmd::RestDone => {
                self.phase = Phase::Idle;
                self.emit_state("idle");
            }
            Cmd::AlertTimeout => {
                self.alert_timeout();
            }
        }
        self.sync_ui();
    }

    fn alert_timeout(&mut self) {
        match self.phase {
            Phase::AlertFocus { .. } => {
                (self.record)(self.last_focus_min, self.snoozes, ALERT_TIMEOUT_NOTE, self.focus_started_at);
                self.focus_started_at = None;
            }
            Phase::AlertRest => {}
            _ => return,
        }
        self.phase = Phase::Idle;
        self.snoozes = 0;
        self.emit_state("idle");
    }
}

pub fn spawn(
    app: AppHandle,
    record: Box<dyn Fn(u64, u64, &str, Option<SystemTime>) + Send>,
    on_focus_end: Box<dyn Fn(&AppHandle, bool) + Send + Sync>,
    on_rest_end: Box<dyn Fn(&AppHandle) + Send + Sync>,
    on_invalidated: Box<dyn Fn(&AppHandle) + Send + Sync>,
    on_ui: Box<dyn Fn(&str, u64, bool) + Send + Sync>,
) -> TimerHandle {
    let (tx, rx) = channel();
    let mut engine = Engine {
        app,
        rx,
        phase: Phase::Idle,
        snoozes: 0,
        rounds_done: 0,
        last_focus_min: 25,
        last_wall: SystemTime::now(),
        focus_started_at: None,
        record,
        on_focus_end,
        on_rest_end,
        on_invalidated,
        on_ui,
        last_ui: ("idle", 0, false),
    };
    std::thread::Builder::new()
        .name("pomodoro".into())
        .spawn(move || loop {
            engine.tick();
            // 命令立刻处理；无命令则 500ms 后再 tick
            match engine.rx.recv_timeout(Duration::from_millis(500)) {
                Ok(cmd) => engine.handle(cmd),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        })
        .expect("启动番茄钟线程失败");
    TimerHandle(tx)
}
