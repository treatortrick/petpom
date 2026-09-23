/* 设置页：读写 Rust 侧 SQLite 配置，改动即生效（保存后 emit config/changed） */
(function () {
  const { invoke } = window.__TAURI__.core;

  const LIMITS = {
    focus_min: [1, 180, 5], // <5 分钟按 1 分钟步进，≥5 按 5 分钟步进（见 stepValue）
    short_min: [1, 60, 1],
    long_every: [2, 8, 1],
    long_min: [5, 60, 5],
  };

  // 步进计算：focus_min 特殊——1~5 区间步长 1，之后步长 5（…4→5→10→15…）
  function stepValue(k, v, d) {
    const [min, max, step] = LIMITS[k];
    if (k !== "focus_min") return Math.min(max, Math.max(min, v + d * step));
    if (d > 0) return v < 5 ? Math.min(5, v + 1) : Math.min(max, v + 5);
    return v <= 5 ? Math.max(min, v - 1) : (v <= 10 ? 5 : v - 5);
  }

  let cfg = {};

  async function load() {
    cfg = await invoke("get_settings");
    for (const k of Object.keys(LIMITS)) {
      const el = document.getElementById("v-" + k);
      if (el) el.textContent = cfg[k];
    }
    // 形象卡
    document.querySelectorAll("#pet-cards .pc[data-v]").forEach((c) => {
      c.classList.toggle("sel", c.dataset.v === cfg.pet_pack);
    });
    // 开关
    document.querySelectorAll(".tgl").forEach((t) => {
      t.classList.toggle("on", !!cfg[t.dataset.k]);
    });
    // 宠物名
    const nameEl = document.getElementById("pet-name");
    if (nameEl) nameEl.value = cfg.pet_name || "";
  }

  async function save(patch) {
    Object.assign(cfg, patch);
    await invoke("set_settings", { patch });
  }

  // steppers
  document.querySelectorAll(".stepper button").forEach((b) => {
    b.addEventListener("click", async () => {
      const k = b.dataset.k;
      const v = stepValue(k, cfg[k] || 0, Number(b.dataset.d) > 0 ? 1 : -1);
      document.getElementById("v-" + k).textContent = v;
      await save({ [k]: v });
    });
  });

  // 形象卡
  document.querySelectorAll("#pet-cards .pc[data-v]").forEach((c) => {
    c.addEventListener("click", async () => {
      if (c.classList.contains("dis")) return;
      document.querySelectorAll("#pet-cards .pc").forEach((x) => x.classList.remove("sel"));
      c.classList.add("sel");
      await save({ pet_pack: c.dataset.v });
    });
  });

  // 开关
  document.querySelectorAll(".tgl").forEach((t) => {
    t.addEventListener("click", async () => {
      const v = !t.classList.contains("on");
      t.classList.toggle("on", v);
      await save({ [t.dataset.k]: v });
    });
  });

  // 宠物名（interaction-spec §3：trim 后 ≤12 字符，空 = 回到形象默认名）
  const nameInput = document.getElementById("pet-name");
  if (nameInput) {
    nameInput.addEventListener("change", async () => {
      const v = nameInput.value.trim();
      nameInput.value = v;
      await save({ pet_name: v });
    });
  }

  /* ============ 它的小窝 ============ */
  const denGrid = document.getElementById("den-grid");
  function renderDen(d) {
    document.getElementById("den-owned").textContent = d.owned;
    document.getElementById("den-total").textContent = d.total;
    const unseen = document.getElementById("den-unseen");
    unseen.innerHTML = d.unseen > 0 ? `<span class="badge-n">${d.unseen} 新</span>` : "";
    const trip = document.getElementById("den-trip");
    if (d.trip && d.trip.active) {
      const m = Math.ceil(d.trip.remain_sec / 60);
      trip.textContent = `它正在外面转悠，大约 ${m} 分钟后回来。`;
    } else {
      trip.textContent = "投喂一条小鱼干，它就出门去逛逛（回来一定会带点东西）。";
    }
    if (!denGrid) return;
    denGrid.innerHTML = d.cards
      .map((c) => {
        if (!c.owned) {
          return `<div class="dc lock r${c.rarity}"><div class="t">？？？</div>` +
                 `<div class="d">${c.rarity_label} · 还没遇见</div></div>`;
        }
        const day = (c.obtained_at || "").slice(0, 10);
        const isNew = c.seen === 0 ? " new" : "";
        return `<div class="dc r${c.rarity}${isNew}"><div class="t">${c.name}</div>` +
               `<div class="d">${c.text}</div>` +
               `<div class="m">${c.rarity_label} · ${day}</div></div>`;
      })
      .join("");
  }

  async function loadDen() {
    try {
      const d = await invoke("get_den");
      renderDen(d);
      if (d.unseen > 0) {
        await invoke("mark_den_seen");
      }
    } catch (e) {
      if (denGrid) denGrid.innerHTML = `<div class="dc lock"><div class="d">小窝读取失败：${e}</div></div>`;
    }
  }

  load();
  loadDen();
})();
