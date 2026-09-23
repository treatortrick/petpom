/* petpom 宠物窗口引擎：sprite 播放器 + 状态机 + 提醒胶囊 + 摸头交互
 * 资产规格见 ui/assets/petpacks/<pack>/manifest.json
 * 事件协议（Rust → 前端）：
 *   pet/set-pack {pack}          切换形象（前端重新加载 sprite）
 *   pet/state   {state}          直接切状态：idle|focus|happy|sad|pet|eat|micro-*（反应插播）
 *   alert/show  {kind, limited}  kind: "focus-end"|"rest-end"；limited: 婉拒超限只剩两项
 *   alert/hide                   收起胶囊（含超时/选择后）
 *   pet/toast   {text}           顶部 toast（本轮已失效等）
 *   pet/name    {name}           宠物名变更（interaction-spec §3）
 * 前端 → Rust（invoke）：
 *   alert_rest / alert_snooze / alert_skip / alert_timeout / pet_poked / petted_ok
 *   drag_move {x,y}            手动拖拽落位（逻辑坐标）
 *   show_context_menu          右键弹自定义菜单（条件化，见 docs/interaction-spec.md）
 * 注：语音（play_voice / 音频线程）已于 2026-09-15 整体移除，留待后续迭代。
 */
(function () {
  const { invoke } = window.__TAURI__.core;
  const { listen } = window.__TAURI__.event;
  const curWin = window.__TAURI__.window.getCurrentWindow(); // 拖拽用，之前未定义导致拖不动

  const petEl = document.getElementById("pet");
  const bubbleEl = document.getElementById("bubble");
  const pillEl = document.getElementById("pill");
  const pillTitle = document.getElementById("pill-title");
  const pillText = document.getElementById("pill-text");
  const pillBtns = document.getElementById("pill-btns");
  const pillCountdown = document.getElementById("pill-countdown");
  const toastEl = document.getElementById("toast");
  const settleEl = document.getElementById("settle");
  const cardEl = document.getElementById("card-pop");

  // 无障碍（P0-5）：系统开启「减少动态效果」时停掉装饰性动效
  // （呼吸、Zzz、小动作、自转、hover 反应）；承载信息的状态动画保留
  const reduceMotion = window.matchMedia("(prefers-reduced-motion: reduce)").matches;

  let pillLimited = false;      // 当前提醒是否已婉拒超限（只剩两项）
  let lastSteadyState = "idle"; // 摸头动画结束后的回落状态（静止待机）
  let petName = "奶油";         // 宠物名（Rust 侧 get_pet_name，气泡文案用）

  // 回落状态必须真实存在：包里没有该动画（如 tuantuan 无 sit）就落回 idle，
  // 防止 onDone 回落到不存在的 key 被回退逻辑吞掉后卡帧
  function setSteady(key) {
    lastSteadyState = pack && pack.anims[key] ? key : "idle";
  }

  /* ============ sprite 播放器 ============ */
  const PACKS = {
    tuantuan: "assets/petpacks/tuantuan",
    shiba: "assets/petpacks/shiba",
  };
  let pack = null;            // { name, base, anims: {state: {frames:[Image], fps, loop}} }
  let player = null;          // 当前播放器 { state, timer, idx }

  /* manifest 读取（2026-09-23 重构）
   * 主路径：<script src> 加载 manifest.js。
   *   原因：WKWebView 下页面自身所在的自定义协议（tauri://）不支持 fetch/XHR，
   *   实测连 fetch("index.html") 都抛 TypeError: Load failed；而 <script>/<img>
   *   这类子资源加载是正常的。manifest.json 仍是唯一数据源，manifest.js 由
   *   assets/gen_pack_js.py 生成。
   * 回退：fetch manifest.json（开发环境 / 支持 fetch 的运行时）。 */
  function loadScriptManifest(packName, base) {
    return new Promise((resolve, reject) => {
      if (window.__PETPOM_PACKS__ && window.__PETPOM_PACKS__[packName]) {
        resolve(window.__PETPOM_PACKS__[packName]);
        return;
      }
      const s = document.createElement("script");
      s.src = base + "/manifest.js";
      s.onload = () => {
        const m = window.__PETPOM_PACKS__ && window.__PETPOM_PACKS__[packName];
        m ? resolve(m) : reject(new Error("manifest.js 已加载但无该形象数据"));
      };
      s.onerror = () => reject(new Error("manifest.js 加载失败"));
      document.head.appendChild(s);
    });
  }

  async function fetchManifest(packName, base) {
    try {
      return await loadScriptManifest(packName, base);
    } catch (e1) {
      try {
        const r = await fetch(base + "/manifest.json");
        if (r.ok) return await r.json();
        throw new Error("HTTP " + r.status);
      } catch (e2) {
        invoke("frontend_error", {
          msg: "manifest 读取失败: script=" + String(e1) + " fetch=" + String(e2) +
            " | href=" + location.href,
        }).catch(() => {});
        throw e2;
      }
    }
  }

  async function loadPack(name) {
    const base = PACKS[name] || PACKS.shiba;
    const manifest = await fetchManifest(name, base);
    const anims = {};
    // manifest.anims: { 动画名: 帧数 }；旧包没有该字段则按 10 帧加载六个基础动画
    const list = manifest.anims || { walk: 10, pet: 10, sleep: 10, alert: 10, happy: 10, sad: 10 };
    for (const key of Object.keys(list)) {
      const fps = (manifest.fps && manifest.fps[key]) || 8;
      const loop = manifest.loop ? manifest.loop[key] !== false : true;
      const frames = [];
      for (let i = 1; i <= list[key]; i++) {
        const img = new Image();
        img.src = `${base}/anims/${key}/${key}-${String(i).padStart(2, "0")}.png`;
        frames.push(img);
      }
      anims[key] = { frames, fps, loop };
    }
    // 旧包回退：没有真实 idle 动画时用 walk 首帧静帧 + CSS 呼吸
    if (!anims.idle) {
      anims.idle = { frames: [anims.walk.frames[0]], fps: 0, loop: true };
    }
    // states 映射（2026-09-03 修复）：manifest.states 把语义状态映射到动画
    // （focus→sleep 等），此前从未实现导致 play("focus") 静默失败——
    // 专注时无打盹动画，且抚摸/投喂回落的 lastSteadyState="focus" 不存在，
    // 播放器卡死在上一动画最后一帧
    const states = manifest.states || {};
    for (const [k, v] of Object.entries(states)) {
      // 跳过 idle：旧包（tuantuan）states.idle→walk 是旧设计，映射会让
      // "待机像跑步"复发；idle 保持上方静帧+呼吸回退（2026-09-03 复盘）
      if (k === "idle") continue;
      if (v && v.anim && anims[v.anim] && !anims[k]) anims[k] = anims[v.anim];
    }
    // motion：程序补间映射（关键姿势之间的连续运动由 CSS 层驱动）
    pack = { name, base, anims, realIdle: !!manifest.anims, motion: manifest.motion || null };
    // 帧图加载哨兵：图片 404 时没有任何显式报错，只表现为「宠物不显示」，必须留痕
    const probeFrame = anims.idle && anims.idle.frames[0];
    if (probeFrame) {
      probeFrame.addEventListener("error", () => {
        invoke("frontend_error", { msg: "frame load fail: " + name + " " + probeFrame.src })
          .catch(() => {});
      });
    }
    play("idle"); // 默认待机
  }

  // 程序补间：把状态映射成 CSS 动作类（none / breath / doze / sway / bounce / press）
  function motionClass(stateKey) {
    const m = (pack && pack.motion && pack.motion[stateKey]) || "none";
    if (reduceMotion || m === "none") return "motion";
    return "motion m-" + m;
  }

  function play(stateKey, opts = {}) {
    if (!pack) return;
    let spec = pack.anims[stateKey];
    if (!spec) {
      // 未知动画名一律留痕并回退 idle，绝不静默卡死播放器（2026-09-03 复盘：
      // play("focus")/play("sit") 静默 return 是抚摸/投喂不还原的根因）
      invoke("frontend_error", { msg: "play miss: " + stateKey + " pack=" + pack.name }).catch(() => {});
      spec = pack.anims.idle;
      if (!spec) return;
    }
    stopPlayer();
    const motionEl = petEl.querySelector(".motion");
    // 新状态装进独立容器：与旧容器交叉淡入 120ms，消除状态硬切的跳变感
    const box = document.createElement("div");
    box.className = "anim";
    const nodes = spec.frames.map((img) => {
      const n = img.cloneNode();
      n.className = "frame";
      box.appendChild(n);
      return n;
    });
    motionEl.appendChild(box);
    requestAnimationFrame(() => box.classList.add("on"));
    motionEl.querySelectorAll(".anim").forEach((o) => {
      if (o !== box && o.classList.contains("on")) {
        o.classList.remove("on");
        o.classList.add("fading");
        setTimeout(() => o.remove(), 180);
      } else if (o !== box && !o.classList.contains("fading")) {
        o.remove(); // 上一轮淡出中的残留
      }
    });
    // 程序补间层（呼吸/摇摆/跳跃/回弹）
    motionEl.className = motionClass(stateKey);
    const anim = { state: stateKey, idx: 0, nodes, ...opts };
    nodes[0].classList.add("on");
    if (spec.fps > 0 && nodes.length > 1) {
      anim.timer = setInterval(() => {
        nodes[anim.idx].classList.remove("on");
        anim.idx = (anim.idx + 1) % nodes.length;
        nodes[anim.idx].classList.add("on");
        if (anim.idx === nodes.length - 1 && !spec.loop && anim.onDone) {
          const cb = anim.onDone;
          anim.onDone = null;
          cb();
        }
      }, 1000 / spec.fps);
    }
    player = anim;
    // Zzz 只跟「睡觉」绑定（休息态）：专注=看书，不该有 Zzz
    if (spec === pack.anims.sleep) startZzz(); else stopZzz();
  }

  function stopPlayer() {
    if (player && player.timer) clearInterval(player.timer);
    player = null;
  }

  /* ============ Zzz 粒子 ============ */
  let zzzTimer = null;
  function startZzz() {
    stopZzz();
    zzzTimer = setInterval(() => {
      const z = document.createElement("span");
      z.className = "zzz";
      z.textContent = Math.random() > 0.5 ? "Z" : "z";
      z.style.left = 162 + Math.random() * 30 + "px";
      z.style.bottom = 177 + Math.random() * 15 + "px";
      document.getElementById("stage").appendChild(z);
      setTimeout(() => z.remove(), 2500);
    }, 900);
  }
  function stopZzz() {
    if (zzzTimer) clearInterval(zzzTimer);
    zzzTimer = null;
    document.querySelectorAll(".zzz").forEach((n) => n.remove());
  }

  /* ============ 气泡 / toast ============ */
  let bubbleTimer = null;
  function bubble(text, ms = 4000) {
    syncDuck(); // 气泡出现时时间牌避让
    bubbleEl.textContent = text;
    bubbleEl.classList.add("show");
    clearTimeout(bubbleTimer);
    if (ms > 0) bubbleTimer = setTimeout(() => {
      bubbleEl.classList.remove("show");
      syncDuck();
    }, ms);
  }
  function toast(text, ms = 3500) {
    toastEl.textContent = text;
    toastEl.classList.add("show");
    setTimeout(() => toastEl.classList.remove("show"), ms);
  }

  /* ============ 完成结算卡（P0-1） ============
   * 数据源：Rust 在确认休息后 emit 的 stats/settle（本轮时长 + 今日累计 + 亲密度）。
   * 约 5.2 秒淡出；期间时间牌避让，避免两块信息打架。 */
  let settleTimer = null;
  function showSettle(p) {
    settleEl.innerHTML =
      `<div class="s-top">第 ${p.today_rounds} 个番茄完成</div>` +
      `<div class="s-main"><b>${p.round_min}</b><span>分钟</span></div>` +
      `<div class="s-sub">今日累计 ${p.today_min} 分钟 · ${p.bond_title} ${p.bond}</div>` +
      (p.distractions > 0 ? `<div class="s-sub">专注中被我抓到 ${p.distractions} 次开小差</div>` : "") +
      `<div class="s-tail">小鱼干 +1（现在 ×${p.snacks}）</div>`;
    settleEl.classList.add("show");
    syncDuck();
    clearTimeout(settleTimer);
    settleTimer = setTimeout(() => {
      settleEl.classList.remove("show");
      syncDuck();
    }, 5200);
  }

  /* ============ 提醒胶囊 ============ */
  let alerting = false;         // 胶囊是否打开
  let alertKind = null;
  let alertCountdown = null;    // 60s 自动收回
  let alertLeft = 60;

  function showAlert(cfg) {
    alerting = true;
    alertKind = cfg.kind;
    play("alert");
    buildPill(cfg);
    alertLeft = 60;
    renderCountdown();
    clearInterval(alertCountdown);
    alertCountdown = setInterval(() => {
      alertLeft -= 1;
      renderCountdown();
      if (alertLeft <= 0) hideAlert(true);
    }, 1000);
  }

  function buildPill(cfg) {
    const focusEnd = cfg.kind === "focus-end";
    pillTitle.textContent = focusEnd ? "休息一下～" : "该回来工作了";
    pillText.textContent = focusEnd
      ? `你已专注 ${cfg.focus_min || 25} 分钟，起来喝口水，我等你回来。`
      : "休息够啦，回来继续吧，我陪你。";
    const btns = [];
    if (focusEnd) {
      btns.push(mkBtn("好，休息", "main", () => invoke("alert_rest")));
      if (!cfg.limited) {
        btns.push(mkBtn("摸摸它再等5分钟", "snooze", () => invoke("alert_snooze")));
      } else {
        pillText.textContent = "你已经拖了两次啦，我真的有点担心你。";
      }
      btns.push(mkBtn("跳过本轮", "", () => invoke("alert_skip")));
    } else {
      btns.push(mkBtn("好，继续专注", "main", () => invoke("rest_done")));
      btns.push(mkBtn("再躺5分钟", "snooze", () => invoke("rest_snooze")));
      btns.push(mkBtn("跳过本轮", "", () => invoke("alert_skip")));
    }
    pillBtns.innerHTML = "";
    btns.forEach((b) => pillBtns.appendChild(b));
  }

  function mkBtn(label, cls, onClick) {
    const b = document.createElement("div");
    b.className = "pbtn" + (cls ? " " + cls : "");
    b.textContent = label;
    b.addEventListener("click", onClick);
    return b;
  }

  function renderCountdown() {
    pillCountdown.textContent = `${alertLeft} 秒后我先回窝哦`;
  }

  function hideAlert(byTimeout = false) {
    clearInterval(alertCountdown);
    alertCountdown = null;
    alerting = false;
    pillEl.classList.remove("show");
    if (byTimeout) invoke("alert_timeout").catch(() => {});
    play("idle"); // 回静止待机
  }

  /* ============ 摸头 / 拖拽（手动坐标拖拽） ============
   * 用屏幕坐标（e.screenX/Y）计算位移——clientX 会随窗口移动而变，
   * 造成"窗口追上鼠标→目标回退"的反馈震荡（频闪/不跟手）；
   * 屏幕坐标与窗口位置无关，目标位置 = 初始窗口位置 + 指针屏幕位移。
   */
  let pressTimer = null;
  let pressStart = null;   // { t } 按下时间
  let dragBase = null;     // { wx, wy, sx, sy } 窗口逻辑坐标 + 指针屏幕坐标锚点
  let dragging = false;
  let facing = 0;          // 拖拽朝向：-1 左(基准), 1 右(镜像)；0=未拖拽
  let rafPending = false;
  let pendingPos = null;
  const stageEl = document.getElementById("stage");

  // 帧基准朝向=左；向右拖拽时水平镜像（manifest: walk 帧朝向=left）
  function setFacing(dir) {
    if (facing === dir) return;
    facing = dir;
    petEl.style.transform = dir === 1 ? "scaleX(-1)" : "";
  }

  // 诊断探针已移除（拖拽根因=WKWebView 原生图片拖拽，已修复）

  // 原生图片拖拽三重保险之二：dragstart 一律取消（pointer-events:none 之外再兜底，
  // 覆盖未来可能新增的、未设 pointer-events 的图片/链接）
  window.addEventListener("dragstart", (e) => { e.preventDefault(); });

  // 三重保险之三：WebKit 从 mouse 事件路径发起原生拖拽，pointerdown 层取消拦不住，
  // 必须在 mousedown 层取消（不影响胶囊按钮——它们用 click 事件，且已由 closest("#pill") 跳过）
  stageEl.addEventListener("mousedown", (e) => { e.preventDefault(); });

  stageEl.addEventListener("pointerdown", (e) => {
    if (e.button !== 0) return;
    if (e.target.closest("#pill") || e.target.closest("#timer-badge")) return; // 胶囊/时间牌点击不当作抚摸/拖拽
    e.preventDefault(); // 防 webview 原生图片拖拽/文本选中干扰
    try { stageEl.setPointerCapture(e.pointerId); } catch (_) {}
    pressStart = { t: Date.now() };
    pressTimer = setTimeout(() => {
      pressTimer = null;
      petAction();
    }, 900);
    // 异步取窗口当前逻辑坐标 + 记下指针屏幕坐标，组成拖拽锚点
    const sx0 = e.screenX, sy0 = e.screenY;
    curWin.outerPosition().then((p) => {
      const dpr = window.devicePixelRatio || 1;
      dragBase = { wx: p.x / dpr, wy: p.y / dpr, sx: sx0, sy: sy0 };
    }).catch(() => { dragBase = null; });
  });

  stageEl.addEventListener("pointermove", (e) => {
    if (!pressStart && !dragging) return;
    if (!dragging) {
      if (!pressStart) return;
      const dx = e.screenX - (dragBase ? dragBase.sx : e.screenX);
      const dy = e.screenY - (dragBase ? dragBase.sy : e.screenY);
      if (dx * dx + dy * dy <= 64) return; // 8px 内不算拖拽（保留单击抚摸）
      if (pressTimer) { clearTimeout(pressTimer); pressTimer = null; }
      if (!dragBase) return; // 锚点未就绪，本次先不拖
      dragging = true;
      play("walk"); // 被拎着走
    }
    // 朝向跟随拖拽方向（>4px 才切换，避免抖动）
    const mvx = e.screenX - dragBase.sx;
    if (mvx > 4) setFacing(1); else if (mvx < -4) setFacing(-1);
    queueMove(
      Math.round(dragBase.wx + (e.screenX - dragBase.sx)),
      Math.round(dragBase.wy + (e.screenY - dragBase.sy))
    );
  });

  function queueMove(x, y) {
    pendingPos = { x, y };
    if (rafPending) return;
    rafPending = true;
    requestAnimationFrame(() => {
      rafPending = false;
      const p = pendingPos; pendingPos = null;
      if (p && dragging) invoke("drag_move", { x: p.x, y: p.y }).catch(() => {});
    });
  }

  window.addEventListener("pointerup", () => {
    const wasDrag = dragging;
    dragging = false; dragBase = null; pendingPos = null;
    if (pressTimer) { clearTimeout(pressTimer); pressTimer = null; }
    const start = pressStart;
    pressStart = null;
    if (wasDrag) {
      setFacing(0); // 回基准朝向
      play(lastSteadyState || "idle");
      return;
    }
    // 短按（未拖拽、未达长按阈值）= 抚摸；提醒态外只抚摸，不做任何其它逻辑
    if (start && Date.now() - start.t < 900) petAction();
  });

  window.addEventListener("pointercancel", () => {
    dragging = false; dragBase = null; pendingPos = null;
    if (pressTimer) { clearTimeout(pressTimer); pressTimer = null; }
    pressStart = null;
    setFacing(0);
    play(lastSteadyState || "idle");
  });

  /* 悬停反应（P1-12）：驻留 1.5s + 30s 冷却，拖拽/提醒/按压中不触发 */
  stageEl.addEventListener("pointerenter", () => {
    if (reduceMotion) return;
    clearTimeout(hoverTimer);
    hoverTimer = setTimeout(() => {
      if (Date.now() < hoverCooldownUntil) return;
      if (dragging || alerting || pressStart) return;
      if (!pack || !player || !(player.state === "idle" || player.state === "sit")) return;
      const keys = Object.keys(pack.anims).filter((k) => k.startsWith("micro-"));
      if (!keys.length) return;
      hoverCooldownUntil = Date.now() + 30000;
      const k = keys[Math.floor(Math.random() * keys.length)];
      play(k, { onDone: () => { if (player && player.state === k) play(lastSteadyState); } });
    }, 1500);
  });
  stageEl.addEventListener("pointerleave", () => clearTimeout(hoverTimer));

  /* 右键 = 自定义菜单（原生 popup，Rust 侧条件化构建）。
   * 屏蔽 WebView 默认菜单；拖拽中不弹；穿透模式下系统层收不到事件（托盘兜底）。 */
  stageEl.addEventListener("contextmenu", (e) => {
    e.preventDefault();
    if (dragging) return;
    invoke("show_context_menu").catch(() => {});
  });

  function petAction() {
    if (alerting && alertKind === "focus-end") {
      // 摸头 = 婉拒（等效按钮）；超限时摸头无效并吐槽
      if (!pillLimited) {
        bubble("那…再等你 5 分钟哦～");
        invoke("alert_snooze").catch(() => {});
      } else {
        bubble("不能再拖啦，身体要紧嘛。");
      }
      return;
    }
    // 专注中点击 = 「开小差」提醒（2026-09-15 队长拍板）
    // 属于用户主动触发（一触层），不是宠物索要注意力，不违反零打扰纪律
    if (player && player.state === "focus") {
      nudgeOnDistraction();
      return;
    }
    // 非提醒态：纯摸头，pet 动画 1 遍后回原状态 + 随机抚摸反馈（interaction-spec #1）
    play("pet", { onDone: () => play(lastSteadyState) });
    const petLines = [
      "呼噜呼噜…最喜欢你啦～",
      "嘿嘿，被摸头就会开心。",
      "再摸一会儿嘛…",
      "{n}今天也很乖哦。",
      "充电中…{n}的电量+1。",
    ];
    bubble(
      petLines[Math.floor(Math.random() * petLines.length)].replace("{n}", petName),
      3000
    );
  }

  /* ============ 专注中「开小差」提醒 ============
   * 三档递进：① 温柔陪伴 ② 俏皮 ③ 给出口（引导点时间牌暂停）
   * 语气原则：反说教、不说"不许"——宠物是搭子，不是监工。 */
  const NUDGE_LINES = [
    "我在替你守着这 25 分钟呢，一起？",
    "诶，我在这儿呢～再专心一点点？",
    "要不…先暂停一下？点一下我头顶的牌子就好。",
  ];
  let nudgeLevel = 0;
  let nudgeCooldownUntil = 0;
  let badgeHintUntil = 0; // 指引期间强制显示时间牌（否则会被气泡避让逻辑藏掉）

  function nudgeOnDistraction() {
    const now = Date.now();
    if (now < nudgeCooldownUntil) return; // 连点合并，防刷屏
    nudgeCooldownUntil = now + 2000;
    nudgeLevel = Math.min(nudgeLevel + 1, NUDGE_LINES.length);
    // 形象包未提供 focus-nudge 动画（如遗留 V1 包）时保持当前专注动画，
    // 气泡/计数/时间牌指引照常 —— 避免 play miss 回退 idle 打断专注画面
    const nudgeKey = pack && pack.anims["focus-nudge"] ? "focus-nudge" : null;
    if (nudgeKey) {
      play(nudgeKey, { onDone: () => { if (player && player.state === nudgeKey) play("focus"); } });
    }
    bubble(NUDGE_LINES[nudgeLevel - 1], 3800);
    invoke("note_distraction").catch(() => {}); // 计入本轮开小差次数（结算卡展示）
    if (nudgeLevel >= NUDGE_LINES.length) {
      // 第 3 档：把「暂停」出口指出来（时间牌闪烁 + 缩放脉冲）
      badgeHintUntil = Date.now() + 2800;
      badgeEl.classList.add("hint");
      bubbleEl.classList.add("lift");
      syncDuck();
      badgeEl.classList.add("on"); // 立刻恢复可见（不等下一次 tick / 不被气泡隐藏）
      setTimeout(() => {
        badgeEl.classList.remove("hint");
        bubbleEl.classList.remove("lift");
        syncDuck();
      }, 2800);
    }
  }
  function resetNudge() {
    nudgeLevel = 0;
    nudgeCooldownUntil = 0;
  }

  /* ============ idle 小动作随机插播 ============
   * 待机时每 20~40s 随机播一次眨眼/耳朵抖/歪头（各 2 帧：变体+复原），
   * 这是像素宠物「逼真感」的最大杠杆（Tamagotchi/VPet 类产品通用做法）。
   */
  let microTimer = null;
  function scheduleMicro() {
    clearTimeout(microTimer);
    microTimer = setTimeout(() => {
      const keys = pack ? Object.keys(pack.anims).filter((k) => k.startsWith("micro-")) : [];
      if (
        keys.length &&
        player && player.state === "idle" && pack.realIdle &&
        !alerting && !dragging && !document.hidden && !reduceMotion
      ) {
        const k = keys[Math.floor(Math.random() * keys.length)];
        play(k, { onDone: () => { if (player && player.state === k) play("idle"); } });
      }
      scheduleMicro();
    }, 20000 + Math.random() * 20000);
  }

  /* ============ 专注中「自转」（P1-9） ============
   * 专注稳定态本身就是「看书」，所以自转只偶尔换一次「整理书」，
   * 避免和主循环重复；纯氛围：不弹气泡、不发声。 */
  let selfTimer = null;
  function scheduleSelfAction() {
    clearTimeout(selfTimer);
    selfTimer = setTimeout(() => {
      if (
        pack && player && player.state === "focus" &&
        !alerting && !dragging && !document.hidden && !reduceMotion
      ) {
        const keys = ["focus-tidy"].filter((k) => pack.anims[k]);
        if (keys.length) {
          const k = keys[Math.floor(Math.random() * keys.length)];
          play(k, { onDone: () => { if (player && player.state === k) play("focus"); } });
        }
      }
      scheduleSelfAction();
    }, 60000 + Math.random() * 60000);
  }

  /* ============ hover 反应（P1-12） ============
   * 鼠标驻留 1.5 秒触发一次小动作，30 秒冷却——零成本互动但要防烦。 */
  let hoverTimer = null;
  let hoverCooldownUntil = 0;

  /* ============ 休息窗口轻互动（P1-10） ============
   * 休息开始 20 秒后伸个懒腰，仅休息期开放、每次休息仅一次。 */
  let restStretchTimer = null;
  function scheduleRestStretch() {
    clearTimeout(restStretchTimer);
    restStretchTimer = setTimeout(() => {
      if (pack && player && player.state === "sit" && !alerting && !reduceMotion && pack.anims["rest-stretch"]) {
        play("rest-stretch", { onDone: () => { if (player && player.state === "rest-stretch") play(lastSteadyState); } });
      }
    }, 20000);
  }
  function cancelRestStretch() {
    clearTimeout(restStretchTimer);
  }

  /* ============ 头顶时间牌 ============
   * 数据源：timer 引擎本就每秒 emit 的 timer/tick {phase, remaining_sec, paused}
   * 与阶段切换的 timer/state {phase}，前端纯展示，Rust 零改动。
   * 四态：专注橙/休息青/暂停灰/空闲隐藏；最后 60s 转红（与托盘 urgent 同阈值）；
   * 点击=暂停/继续（复用 pause_resume 命令）；气泡出现时整牌避让。 */
  const badgeEl = document.getElementById("timer-badge");
  const badgeLabel = badgeEl.querySelector(".t-label");
  const badgeTime = badgeEl.querySelector(".t-time");

  function fmtMMSS(sec) {
    const m = Math.floor(sec / 60), s = sec % 60;
    return String(m).padStart(2, "0") + ":" + String(s).padStart(2, "0");
  }
  function renderBadge(phase, remain, paused) {
    const show = phase === "focus" || phase === "rest";
    if (!show) { badgeEl.classList.remove("on"); return; }
    // 第 3 档指引期间强制显示：气泡避让与每秒刷新都不该把牌藏掉
    const forceOn = Date.now() < badgeHintUntil;
    badgeEl.classList.toggle("on", forceOn || !bubbleEl.classList.contains("show"));
    badgeEl.classList.toggle("rest", phase === "rest");
    badgeEl.classList.toggle("paused", paused);
    badgeEl.classList.toggle("urgent", !paused && phase === "focus" && remain <= 60);
    badgeLabel.textContent = phase === "rest" ? "休息中" : (paused ? "已暂停" : "专注中");
    badgeTime.textContent = fmtMMSS(remain);
  }
  /* 避让改为「由当前可见元素推导」而不是计数：
     2026-09-23 修复——气泡与结算卡同时出现时，先结束的那个会 badgeDuck(false)，
     把另一个还在显示时本该继续隐藏的时间牌提前放出来。
     第 3 档指引（badgeHintUntil）期间即便有气泡也要保持可见。 */
  function syncDuck() {
    const covered =
      bubbleEl.classList.contains("show") || settleEl.classList.contains("show") ||
      cardEl.classList.contains("show");
    badgeEl.classList.toggle("duck", covered && Date.now() >= badgeHintUntil);
  }
  badgeEl.addEventListener("pointerdown", (e) => e.stopPropagation());
  badgeEl.addEventListener("click", () => {
    invoke("pause_resume").catch(() => {});
  });

  /* ============ Rust 事件 ============ */
  /* ============ 探险带回的卡片（温和期待型的核心反馈） ============
   * 它回来时：播 happy + 弹出卡片（稀有度/名字/它想说的话）+ 气泡。
   * 离线产出：app 没开时也在走时间，启动补结算的卡片会带 startup 标记。 */
  let cardTimer = null;
  function showCard(p) {
    const rare = (p.rarity || 0) >= 2;
    cardEl.className = ""; // 复位
    cardEl.innerHTML =
      `<div class="c-rarity">${p.rarity_label || ""} · 它带回来的</div>` +
      `<div class="c-name">${p.name}</div>` +
      `<div class="c-text">${p.text}</div>` +
      `<div class="c-foot">已收进它的小窝（右键菜单 → 设置 → 小窝）</div>`;
    if (rare) cardEl.classList.add("rare");
    void cardEl.offsetWidth;
    cardEl.classList.add("show");
    syncDuck();
    clearTimeout(cardTimer);
    cardTimer = setTimeout(() => {
      cardEl.classList.remove("show");
      syncDuck();
    }, 7000);
  }

  function onCardReturn(p) {
    play("happy", { onDone: () => play(lastSteadyState) });
    const lead = p.startup ? "我回来啦！" : "我回来啦！";
    bubble(`${lead}给你带了「${p.name}」`, 4200);
    showCard(p);
  }

  async function setup() {
    await listen("pet/set-pack", (e) => {
      loadPack(e.payload.pack);
    });
    await listen("pet/state", (e) => {
      const s = e.payload.state === "walk" ? "idle" : e.payload.state;
      if (s !== "rest") cancelRestStretch(); // 离开休息态取消未触发的伸懒腰
      if (s === "rest") {
        // 休息：先 happy 播一遍庆祝，再落定到睡觉循环（旧包无 rest 映射则回 sit）
        setSteady(pack.anims.rest ? "rest" : "sit");
        play("happy", { onDone: () => play(lastSteadyState) });
        scheduleRestStretch(); // 休息期轻互动（P1-10）
      } else if (s.startsWith("micro-")) {
        // 反应插播（interaction-spec #3/#4）：播一遍回原状态，不改 lastSteadyState
        if (pack.anims[s]) play(s, { onDone: () => play(lastSteadyState) });
      } else if (s === "eat") {
        // 投喂（interaction-spec #9）：eat 资产未到位时回退 happy
        const key = pack.anims.eat ? "eat" : "happy";
        play(key, { onDone: () => play(lastSteadyState) });
      } else if (s === "happy" || s === "sad") {
        setSteady("idle");
        play(s, { onDone: () => play(lastSteadyState) });
      } else {
        setSteady(s === "focus" ? "focus" : "idle");
        if (s === "focus") resetNudge(); // 新一轮专注：开小差档位归零
        play(s);
      }
    });
    await listen("alert/show", (e) => {
      pillLimited = !!e.payload.limited;
      setSteady("idle");
      showAlert(e.payload);
      pillEl.classList.add("show");
    });
    await listen("alert/hide", () => {
      if (alerting) hideAlert(false);
    });
    await listen("pet/toast", (e) => toast(e.payload.text));
    await listen("pet/bubble", (e) => bubble(e.payload.text, e.payload.ms || 4000));
    await listen("pet/name", (e) => { petName = e.payload.name || petName; });
    await listen("timer/tick", (e) => {
      const p = e.payload;
      renderBadge(p.phase, p.remaining_sec, p.paused);
    });
    await listen("timer/state", (e) => {
      // 阶段切换兜底：空闲/提醒态立即收牌（提醒胶囊已承载时间信息）
      const p = e.payload.phase || "";
      if (p === "idle" || p.startsWith("alert-")) badgeEl.classList.remove("on");
    });
    await listen("stats/settle", (e) => showSettle(e.payload));
    await listen("pet/card", (e) => onCardReturn(e.payload));

    try {
      const packName = await invoke("get_current_pack");
      await loadPack(packName);
      try { petName = (await invoke("get_pet_name")) || petName; } catch (_) {}
      invoke("frontend_ready", { info: "pack=" + packName }).catch(
        (e) => invoke("frontend_error", { msg: "ready-fail: " + String(e) }).catch(() => {})
      );
    } catch (e) {
      // 前端初始化失败必须留痕（sprite 不显示时查 /tmp/petpom-app.log）
      try { invoke("frontend_error", { msg: String(e) }); } catch (_) {}
    }
  }

  window.addEventListener("error", (ev) => {
    try { invoke("frontend_error", { msg: "uncaught: " + ev.message }); } catch (_) {}
  });

  setup();
  scheduleMicro();
  scheduleSelfAction();
})();
