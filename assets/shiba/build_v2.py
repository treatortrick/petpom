#!/usr/bin/env python3
"""PetPom 动画资产管线 · V2（2026-09-22 重建，与 docs/animation-spec.md §3/§4 对齐）

用途：把「图生视频」产出的源帧（白底/带水印/带阴影）处理成 PetPom 运行时精灵图：
      抹水印 → 抠底(floodFill) → 去阴影/碎块 → 面积归一 → 共享调色板量化 → 画布贴底居中。

规格要点（来自 animation-spec.md，含 2026-09-15 踩坑修复）：
  1. 调色板距离必须用 int32 计算：int16 下 255²=65025 溢出为负，argmin 会选错色板项。
  2. 锚点模式分动作选：伸展/趴卧类用 max（最高姿势才是标准姿态）；其余用 median。
     用 median 锚定会把坐姿帧放大 26%（rest-stretch 踩坑）。
  3. 面积归一：scale ∝ 1/√area，跨姿势尺寸一致（低头吃碗/趴卧用身高归一会失真）。
  4. 画布 192×192 原生（V1 为 176×150），脚底贴画布底边、躯干质心水平居中。
  5. 循环类动画首尾帧像素差 < 5/255（--check-loop 校验 / --pick-loop 选帧）。
  6. alpha 二值化（>140 → 255）；背景 floodFill(FIXED_RANGE) 抠底（顺带抹贴边水印）。

依赖：python3 -m venv .venv && .venv/bin/pip install pillow numpy
用法：
  # V2（现行标准：192×192 · 40 色共享调色板 · 面积归一）
  .venv/bin/python assets/shiba/build_v2.py \
      --src 源帧目录/ --out ui/assets/petpacks/shiba/anims \
      [--palette-size 40] [--target-area 9000] \
      [--anchor rest-stretch=max,sleep=max] [--pick-loop 6] [--force]

  # V1 遗留兼容（= build_new6.py：176×150 · 26 色 · 身高锚点 96/80）
  .venv/bin/python assets/shiba/build_v2.py --legacy-v1 \
      --src 源帧目录/ --out ui/assets/petpacks/tuantuan/anims
"""

import argparse
import sys
from pathlib import Path

import numpy as np
from PIL import Image

# ---------- 常量（与 animation-spec.md §3 对齐） ----------
ALPHA_BINARY_THRESHOLD = 140   # alpha 二值化：>140 → 255
FLOOD_TOLERANCE = 60           # 背景 floodFill 容差（FIXED_RANGE）
SHADOW_VALUE_MAX = 46          # 去阴影：贴背景的暗像素阈值
SPECK_AREA_MIN = 6             # 碎块最小面积（小于则清除）
PALETTE_MAX_SAMPLES = 60000    # 建调色板采样上限
PALETTE_ITERS = 18             # k-means 迭代次数
LOOP_SEAM_MAX_RATIO = 1.6      # 循环接缝红线：接缝不得大于「正常一步」的 1.6 倍
# ↑ 2026-10-15 起用比值判定。原来那条 <5/255 的首尾帧差红线对稀疏关键帧动画（4~6 帧）
#   根本不可能满足（相邻帧差本身就有几十），而且旧实现比的是 alpha 通道 → 永远返回 0.00
CORNER_MAX_SPREAD = 30         # 四角背景色允许的最大离散度（超过则不敢抠底）
BG_NEAR_WHITE_SUM = 680        # 已废弃（2026-09-23 改为通用背景判定，保留仅作参考）
BG_NEAR_WHITE_SPREAD = 50      # 判定背景为近白：max-min ≤ 此值
BG_EDGE_MIN_COUNT = 50         # 边缘不透明像素少于该数则跳过抠底（透明底帧）
BG_EDGE_COVERAGE = 0.5         # 主色覆盖边缘比例低于该值则跳过抠底（贴边精灵）

# 身高锚点（V1 legacy，§3.2）：站立/坐姿 96px，趴卧/蜷睡 80px
V1_HEIGHT_ANCHOR_DEFAULT = 96
V1_HEIGHT_ANCHOR_PRON_E = {"sleep", "rest-stretch", "eat"}
WORK_MAX_SIDE = 384          # 源帧工作尺寸上限（见 downscale_to_work_size）
# 面积归一目标（--target-area）。注意两点：
# 1) 它是「工作尺寸（384）下的不透明像素数」，不是最终 192 画布上的面积；
# 2) 不同形象体型不同，同一个 target 会得到不同身高——柴犬比团团壮实，
#    用同一个值时团团会偏大。现包资产（工作区管线产出）的标定：
#      柴犬 idle：最终面积 8022px² / 身高 112px
#      团团 idle：最终面积 6893px² / 身高 111px
#    重跑时用 --target-area 微调，以 idle 身高 ≈112px 为准（跨包看起来才一样大）。


def load_rgba(path: Path) -> np.ndarray:
    return np.asarray(Image.open(path).convert("RGBA"), dtype=np.uint8)


def downscale_to_work_size(arr: np.ndarray, max_side: int) -> np.ndarray:
    """把源帧降到工作尺寸（最长边 ≤ max_side），避免区域生长在大图上卡死。

    最终精灵只有 192px 画布、身高约 112px，源帧 1440px 完全是浪费。
    用 LANCZOS 降采样，像素艺术的大色块结构不受影响。
    """
    h, w = arr.shape[:2]
    if max(h, w) <= max_side or max_side <= 0:
        return arr
    scale = max_side / float(max(h, w))
    nw, nh = max(1, round(w * scale)), max(1, round(h * scale))
    img = Image.fromarray(arr, "RGBA").resize((nw, nh), Image.LANCZOS)
    return np.asarray(img, dtype=np.uint8)


def save_rgba(arr: np.ndarray, path: Path) -> None:
    Image.fromarray(arr, "RGBA").save(path)


def drop_specks(rgb: np.ndarray, alpha: np.ndarray) -> np.ndarray:
    """删掉与主体分离的小碎块（水印残留、背景噪点）。

    为什么需要：i2v 视频的水印位置不固定（我们只裁了右下角那条带），
    新视频的水印跑到左上角，抠底后留下一块 20 多像素的灰斑，悬在宠物头边很明显。
    规则保守，避免误删合法的小部件（尾巴尖、爪子、飞溅）：
      只删「面积 < 主体 1%」且「距主体 > 4px」的连通块。
    """
    import cv2
    m = (alpha > ALPHA_BINARY_THRESHOLD).astype(np.uint8)
    if m.sum() == 0:
        return alpha
    n, labels, stats, _ = cv2.connectedComponentsWithStats(m, 8)
    if n <= 2:
        return alpha
    areas = [(int(stats[i, cv2.CC_STAT_AREA]), i) for i in range(1, n)]
    areas.sort(reverse=True)
    main_area, main_idx = areas[0]
    main_mask = (labels == main_idx).astype(np.uint8)
    near = cv2.dilate(main_mask, np.ones((9, 9), np.uint8), iterations=1) > 0  # 主体外扩 4px
    out = alpha.copy()
    for area, idx in areas[1:]:
        if area >= max(8, main_area * 0.01):
            continue  # 够大，可能是合法部件，保留
        comp = labels == idx
        if (comp & near).any():
            continue  # 紧贴主体，保留
        out[comp] = 0
    return out


def bg_reference(rgb: np.ndarray, alpha: np.ndarray) -> np.ndarray | None:
    """判定背景参考色；判不出就返回 None（不抠底）。

    2026-09-23 重写：旧版要求「背景必须是近白」（RGB 和 ≥680），而图生视频的源帧
    背景是**中灰**（实测 174,168,166，和 508）→ 直接跳过抠底、整帧保持不透明，
    表现为「宠物被一个不透明方块包住」。现在改成通用判定：
      1) 四角必须都是不透明的（透明底帧 → 跳过，交给后续步骤）
      2) 四角颜色必须彼此接近（离散度 ≤ CORNER_MAX_SPREAD）——
         主体压到角上时四角不一致，就不敢抠，避免误删主体
      3) 该颜色需在画面边缘占优（≥ BG_EDGE_COVERAGE），否则视为贴边精灵
    不再限制「近白」，白底/灰底/彩底都能处理。
    """
    h, w = alpha.shape
    k = max(2, min(h, w) // 64)
    corners_rgb = [rgb[:k, :k], rgb[:k, -k:], rgb[-k:, :k], rgb[-k:, -k:]]
    corners_a = [alpha[:k, :k], alpha[:k, -k:], alpha[-k:, :k], alpha[-k:, -k:]]
    if min(float(c.mean()) for c in corners_a) <= ALPHA_BINARY_THRESHOLD:
        return None  # 有透明角落 → 已经是抠好的帧
    meds = np.array([np.median(c.reshape(-1, 3), axis=0) for c in corners_rgb])
    if int((meds.max(axis=0) - meds.min(axis=0)).max()) > CORNER_MAX_SPREAD:
        return None  # 四角不一致 → 主体占角，不敢抠
    ref = np.median(meds, axis=0).astype(np.int32)
    edge = np.concatenate([rgb[0, :], rgb[-1, :], rgb[:, 0], rgb[:, -1]], axis=0)
    edge_a = np.concatenate([alpha[0, :], alpha[-1, :], alpha[:, 0], alpha[:, -1]])
    opaque = edge[edge_a > ALPHA_BINARY_THRESHOLD]
    if len(opaque) < BG_EDGE_MIN_COUNT:
        return None
    near = (np.abs(opaque.astype(np.int32) - ref).sum(axis=1) <= FLOOD_TOLERANCE).mean()
    if near < BG_EDGE_COVERAGE:
        return None
    return ref


def floodfill_bg(rgb: np.ndarray, alpha: np.ndarray, ref: np.ndarray, tol: int) -> np.ndarray:
    """从四条边缘 floodFill 标记与背景连通、颜色接近 ref 的像素（FIXED_RANGE）。

    与背景连通的水印（通常贴边）会被一并抹掉。"""
    h, w = alpha.shape
    within = (np.abs(rgb.astype(np.int32) - ref).sum(axis=2) <= tol) | \
             (alpha <= ALPHA_BINARY_THRESHOLD)
    seed = np.zeros((h, w), dtype=bool)
    seed[0, :] = within[0, :]
    seed[-1, :] = within[-1, :]
    seed[:, 0] = within[:, 0]
    seed[:, -1] = within[:, -1]
    return region_grow(seed, within, max(h, w))


def dilate_mask(mask: np.ndarray, radius: int) -> np.ndarray:
    """纯 numpy 方形膨胀（半径 ≤2 足够本资产使用）。"""
    h, w = mask.shape
    out = mask.copy()
    for dy in range(-radius, radius + 1):
        for dx in range(-radius, radius + 1):
            if dy == 0 and dx == 0:
                continue
            src_y = np.arange(max(0, -dy), min(h, h - dy))
            src_x = np.arange(max(0, -dx), min(w, w - dx))
            dst_y = src_y + dy
            dst_x = src_x + dx
            out[np.ix_(dst_y, dst_x)] |= mask[np.ix_(src_y, src_x)]
    return out


def region_grow(seed: np.ndarray, candidate: np.ndarray, max_radius: int) -> np.ndarray:
    """向量化区域生长：从 seed 出发，沿 candidate 内迭代膨胀，返回可达区域。

    替代逐像素 BFS（对 192×192 帧逐像素 Python 循环过慢），
    每次迭代 = 一次 O(9·hw) 的矢量膨胀，迭代数 ≈ 区域半径。"""
    visited = seed & candidate
    frontier = visited.copy()
    for _ in range(max_radius):
        grown = dilate_mask(frontier, 1) & candidate & ~visited
        if not grown.any():
            break
        visited |= grown
        frontier = grown
    return visited


def connected_components(mask: np.ndarray) -> np.ndarray:
    """8-连通分量标记（区域生长实现），0 = 背景。"""
    h, w = mask.shape
    labels = np.zeros((h, w), dtype=np.int32)
    cur = 0
    unlabeled = mask.copy()
    while unlabeled.any():
        ys, xs = np.where(unlabeled)
        seed = np.zeros((h, w), dtype=bool)
        seed[ys[0], xs[0]] = True
        comp = region_grow(seed, mask, max(h, w))
        cur += 1
        labels[comp] = cur
        unlabeled &= ~comp
    return labels


def clean_frame(rgb: np.ndarray, alpha: np.ndarray) -> np.ndarray:
    """一帧完整清理：抠底 → alpha 二值化 → 去阴影/碎块。返回新 alpha。"""
    ref = bg_reference(rgb, alpha)
    a = alpha.copy()
    if ref is not None:
        is_bg = floodfill_bg(rgb, a, ref, FLOOD_TOLERANCE)
        a[is_bg] = 0
    a[a > ALPHA_BINARY_THRESHOLD] = 255
    a[a <= ALPHA_BINARY_THRESHOLD] = 0

    # 去阴影：透明区膨胀一圈，落在带内且极暗的像素变透明
    edge = dilate_mask(a == 0, 1) & (a > 0)
    gray = rgb.astype(np.int32).mean(axis=2)
    a[edge & (gray < SHADOW_VALUE_MAX)] = 0

    # 去碎块
    opaque = a > 0
    labels = connected_components(opaque)
    if labels.max() > 0:
        sizes = np.bincount(labels.ravel())
        for lab in range(1, labels.max() + 1):
            if sizes[lab] < SPECK_AREA_MIN:
                a[labels == lab] = 0
    return a


def bbox_height(a: np.ndarray) -> int:
    ys = np.where(a > 0)[0]
    return int(ys.max() - ys.min() + 1) if len(ys) else 0


def place_on_canvas(rgb: np.ndarray, a: np.ndarray, factor: float,
                    canvas_w: int, canvas_h: int) -> tuple:
    """缩放后贴到 canvas 画布：脚底贴底、水平居中（越界部分裁剪）。"""
    h, w = a.shape
    # 因子上限：保证不超出画布
    max_factor = min(canvas_h / max(1, h), canvas_w / max(1, w))
    factor = min(factor, max_factor)
    nh, nw = max(1, round(h * factor)), max(1, round(w * factor))
    img = Image.fromarray(np.dstack([rgb, a])).resize((nw, nh), Image.LANCZOS)
    alpha = np.asarray(img.split()[3], dtype=np.uint8)
    rgb2 = np.asarray(img.convert("RGB"), dtype=np.uint8)
    out_rgb = np.zeros((canvas_h, canvas_w, 3), dtype=np.uint8)
    out_a = np.zeros((canvas_h, canvas_w), dtype=np.uint8)
    x0 = (canvas_w - nw) // 2
    y0 = canvas_h - nh
    sx0, sy0 = max(0, -x0), max(0, -y0)   # 源图裁剪起点
    dx0, dy0 = max(0, x0), max(0, y0)     # 画布粘贴起点
    w_eff = min(nw, canvas_w - dx0)
    h_eff = min(nh, canvas_h - dy0)
    if w_eff > 0 and h_eff > 0:
        out_rgb[dy0:dy0 + h_eff, dx0:dx0 + w_eff] = rgb2[sy0:sy0 + h_eff, sx0:sx0 + w_eff]
        out_a[dy0:dy0 + h_eff, dx0:dx0 + w_eff] = alpha[sy0:sy0 + h_eff, sx0:sx0 + w_eff]
    return out_rgb, out_a


def union_bbox(frames: list) -> tuple | None:
    """所有帧不透明区域的并集 bbox —— 给「位移动画」用（见 --travel）。"""
    ys, xs = [], []
    for _rgb, a in frames:
        m = a > ALPHA_BINARY_THRESHOLD
        if m.any():
            yy, xx = np.nonzero(m)
            ys += [int(yy.min()), int(yy.max())]
            xs += [int(xx.min()), int(xx.max())]
    if not ys:
        return None
    return min(ys), max(ys), min(xs), max(xs)


def build_palette(frames: list, k: int) -> np.ndarray:
    """跨全部帧采样做 k-means（k-means++ 初始化），返回 k×3 共享调色板。"""
    pixels = []
    for rgb, a in frames:
        ys, xs = np.where(a > 0)
        sub = rgb[ys, xs].astype(np.float64)
        if len(sub):
            stride = max(1, len(sub) // (PALETTE_MAX_SAMPLES // max(1, len(frames))))
            pixels.append(sub[::stride])
    if not pixels:
        raise SystemExit("错误：没有不透明像素，无法建调色板")
    data = np.concatenate(pixels)
    if len(data) > PALETTE_MAX_SAMPLES:
        rng = np.random.default_rng(0)
        data = data[rng.choice(len(data), PALETTE_MAX_SAMPLES, replace=False)]
    rng = np.random.default_rng(0)
    centers = np.empty((k, 3), dtype=np.float64)
    centers[0] = data[rng.integers(0, len(data))]
    for i in range(1, k):
        d2 = ((data[:, None, :] - centers[None, :i, :]) ** 2).sum(axis=2).min(axis=1)
        centers[i] = data[rng.choice(len(data), p=d2 / d2.sum())]
    for _ in range(PALETTE_ITERS):
        assign = ((data[:, None, :] - centers[None, :, :]) ** 2).sum(axis=2).argmin(axis=1)
        for c in range(k):
            sel = assign == c
            if sel.any():
                centers[c] = data[sel].mean(axis=0)
    return np.clip(centers.round(), 0, 255).astype(np.uint8)


def quantize(rgb: np.ndarray, a: np.ndarray, palette: np.ndarray) -> np.ndarray:
    """逐像素映射到最近调色板色。

    距离平方必须全程 int32：255²=65025 在 int16 溢出为负 → argmin 选错色板项、
    浅色被映射成黑块（2026-09-15 踩坑，见 animation-spec §4）。"""
    out = rgb.copy()
    ys, xs = np.where(a > 0)
    if len(ys) == 0:
        return out
    px = rgb[ys, xs].astype(np.int32)
    pal = palette.astype(np.int32)
    d2 = ((px[:, None, :] - pal[None, :, :]) ** 2).sum(axis=2, dtype=np.int32)
    out[ys, xs] = palette[d2.argmin(axis=1)]
    return out


def loop_diff(frames: list) -> float:
    """循环接缝质量：接缝（末帧→首帧）相对「正常一步」的倍数。

    2026-10-16 修：原来的实现是 `frames[0][1]` —— 而这里传入的元素是 (H,W,4) 的 RGBA 数组，
    于是 `[1]` 取到的是**图像的第 1 行**，比的是两帧第一行像素 → 永远返回 0.00「OK」。
    这条「循环红线」因此从来没有校验过任何东西（静默失效比没有检查更危险）。
    现在正确地比较整幅 RGB，并改用比值判定：稀疏关键帧动画（4~6 帧）相邻帧差本来就有几十，
    接缝不可能 <5/255；真正该看的是**接缝是否约等于正常一步**（>1.6× 才算偏大）。
    """
    if len(frames) < 2:
        return 0.0
    first, last = frames[0], frames[-1]
    mask = (first[:, :, 3] > 0) | (last[:, :, 3] > 0)
    if not mask.any():
        return 0.0
    seam = float(
        np.abs(first[:, :, :3].astype(np.int32) - last[:, :, :3].astype(np.int32))[mask].mean()
    )
    steps = [
        float(
            np.abs(
                frames[i][:, :, :3].astype(np.int32) - frames[i - 1][:, :, :3].astype(np.int32)
            )[mask].mean()
        )
        for i in range(1, len(frames))
    ]
    avg = float(np.mean(steps)) if steps else 0.0
    return (seam / avg) if avg > 0.01 else 0.0


def pick_loop_frames(frames: list, n: int) -> list:
    """从长序列中挑 n 张连续帧，使首尾帧差最小（循环闭合选帧）。"""
    if len(frames) <= n:
        return frames
    best_i, best = 0, None
    for i in range(len(frames) - n + 1):
        win = frames[i:i + n]
        # 2026-10-16 修：原实现比 win[0][1]（alpha 通道），所有帧轮廓相同 → d 恒为 0，
        # 于是一律取第一个窗口。改为比 RGB（只在非透明区比较）。
        m = (win[0][1] > 0) | (win[-1][1] > 0)
        if not m.any():
            continue
        d = float(
            np.abs(
                win[0][0][:, :, :3].astype(np.int32) - win[-1][0][:, :, :3].astype(np.int32)
            )[m].mean()
        )
        if best is None or d < best:
            best_i, best = i, d
    return frames[best_i:best_i + n]


def process_anim(src_dir: Path, out_dir: Path, *, canvas_w: int, canvas_h: int,
                 palette_size: int, norm: str, target: float, anchor: str,
                 anchor_h: int, pick_n: int, check_loop: bool, force: bool,
                 travel: bool = False) -> None:
    anim = src_dir.name
    files = sorted(src_dir.glob(f"{anim}-*.png"))
    if not files:
        print(f"  ⚠️ {anim}: 无源帧，跳过")
        return
    frames = []
    for f in files:
        arr = load_rgba(f)
        # 2026-09-23 修复：`region_grow` 是纯 numpy 的迭代式区域生长，
        # 复杂度 ≈ O(迭代次数 × 像素数)。图生视频的源帧是 1440×1440（约 200 万像素），
        # 迭代上限 max(h,w)=1440 → 整条管线会卡死（表现为进程无输出被强杀）。
        # 最终精灵只有 192px 画布、身高约 112px，先降到工作尺寸再处理：
        # 像素数降到 1/8 左右，质量完全够，速度提升一个量级。
        arr = downscale_to_work_size(arr, WORK_MAX_SIDE)
        cleaned = clean_frame(arr[:, :, :3], arr[:, :, 3])
        cleaned = drop_specks(arr[:, :, :3], cleaned)  # 清掉水印/噪点残留
        frames.append((arr[:, :, :3], cleaned))
    if pick_n > 0:
        frames = pick_loop_frames(frames, pick_n)

    # 位移动画（出门/回来）：所有帧共用同一裁剪窗口，保留帧间位移。
    # 否则逐帧居中会把「走出去」压成「原地摆动」（2026-09-23 踩坑）
    if travel:
        ub = union_bbox(frames)
        if ub:
            pad = 4
            y0, y1, x0, x1 = ub
            h0, w0 = frames[0][1].shape
            y0, y1 = max(0, y0 - pad), min(h0 - 1, y1 + pad)
            x0, x1 = max(0, x0 - pad), min(w0 - 1, x1 + pad)
            frames = [(rgb[y0:y1 + 1, x0:x1 + 1], a[y0:y1 + 1, x0:x1 + 1]) for rgb, a in frames]

    # 归一化：V2 面积归一（同动画共用一个缩放因子，参考帧=max/median）；
    # V1 身高锚点（每帧身高归一到 anchor_h，如 96/80）
    if norm == "area":
        areas = np.array([int((a > 0).sum()) for _, a in frames], dtype=np.float64)
        ref = float(areas.max() if (anchor == "max" or travel) else np.median(areas))
        if ref <= 0:
            print(f"  ⚠️ {anim}: 清理后无内容，跳过")
            return
        factor = float(np.sqrt(target / ref))
        scaled = [place_on_canvas(rgb, a, factor, canvas_w, canvas_h) for rgb, a in frames]
    else:  # height
        scaled = []
        for rgb, a in frames:
            hh = bbox_height(a)
            f = anchor_h / hh if hh > 0 else 1.0
            scaled.append(place_on_canvas(rgb, a, f, canvas_w, canvas_h))

    pal = build_palette(scaled, palette_size)
    out = [np.dstack([quantize(rgb, a, pal), a]) for rgb, a in scaled]

    diff = loop_diff(out)
    if check_loop or diff >= LOOP_SEAM_MAX_RATIO:
        flag = "OK" if diff < LOOP_SEAM_MAX_RATIO else f"接缝偏大(≥{LOOP_SEAM_MAX_RATIO}×)"
        print(f"  ℹ️  {anim}: 循环接缝/步长 = {diff:.2f}× [{flag}]")

    dest = out_dir / anim
    dest.mkdir(parents=True, exist_ok=True)
    written = 0
    for i, arr in enumerate(out, 1):
        p = dest / f"{anim}-{i:02d}.png"
        if p.exists() and not force:
            continue
        save_rgba(arr, p)
        written += 1
    tag = f"{written} 帧写入" if written else "全部已存在(--force 覆盖)"
    print(f"  ✅ {anim}: {len(out)} 帧 · {palette_size} 色 · {canvas_w}×{canvas_h} · {tag}")


def main_with_argv(argv: list) -> None:
    ap = argparse.ArgumentParser(description="PetPom 动画资产管线 V2（含 --legacy-v1 兼容）")
    ap.add_argument("--src", required=True, type=Path, help="源帧根目录，含 <anim>/<anim>-NN.png 子目录")
    ap.add_argument("--out", required=True, type=Path, help="输出根目录（写入 <out>/<anim>/<anim>-NN.png）")
    ap.add_argument("--legacy-v1", action="store_true", help="V1 遗留模式：176×150 · 26 色 · 身高锚点（= build_new6.py）")
    ap.add_argument("--canvas-w", type=int, default=None, help="画布宽（默认 V2=192 / V1=176）")
    ap.add_argument("--canvas-h", type=int, default=None, help="画布高（默认 V2=192 / V1=150）")
    ap.add_argument("--palette-size", type=int, default=None, help="调色板颜色数（默认 V2=40 / V1=26）")
    ap.add_argument("--target-area", type=int, default=None, help="标准姿态不透明区目标面积（默认 V2=9000 / V1=6400）")
    ap.add_argument("--anchor", type=str, default="", help="面积归一参考帧覆盖，逗号分隔，如 rest-stretch=max,sleep=max")
    ap.add_argument("--check-loop", action="store_true", help="打印各动画首尾帧差")
    ap.add_argument("--pick-loop", type=int, default=0, help="从长序列中挑 N 张使首尾帧差最小的连续帧")
    ap.add_argument("--force", action="store_true", help="覆盖已存在输出")
    ap.add_argument("--travel", type=str, default="",
                    help="位移动画（逗号分隔，如 leave,return）：所有帧共用裁剪窗口，保留帧间位移")
    args = ap.parse_args(argv)

    legacy = args.legacy_v1
    canvas_w = args.canvas_w or (176 if legacy else 192)
    canvas_h = args.canvas_h or (150 if legacy else 192)
    palette_size = args.palette_size or (26 if legacy else 40)
    target = float(args.target_area or (6400 if legacy else 9000))
    travels = {x.strip() for x in args.travel.split(",") if x.strip()}
    overrides = {}
    for kv in args.anchor.split(","):
        if kv.strip():
            k, v = kv.strip().split("=")
            overrides[k] = v

    print(f"模式: {'V1 遗留' if legacy else 'V2 现行'} · 画布 {canvas_w}×{canvas_h} · "
          f"{palette_size} 色 · 目标面积 {int(target)}")

    anim_dirs = sorted(d for d in args.src.iterdir() if d.is_dir())
    if not anim_dirs:
        raise SystemExit(f"错误：{args.src} 下没有动画子目录（应为 <anim>/<anim>-NN.png）")

    for ad in anim_dirs:
        anim = ad.name
        anchor = overrides.get(anim, "max" if anim in ("rest-stretch", "sleep", "eat") else "median")
        anchor_h = 80 if (legacy and anim in V1_HEIGHT_ANCHOR_PRON_E) else V1_HEIGHT_ANCHOR_DEFAULT
        process_anim(
            ad, args.out,
            canvas_w=canvas_w, canvas_h=canvas_h, palette_size=palette_size,
            norm="height" if legacy else "area", target=target,
            anchor=anchor, anchor_h=anchor_h, pick_n=args.pick_loop,
            check_loop=args.check_loop, force=args.force,
            travel=anim in travels,
        )
    print("完成。")


def main() -> None:
    main_with_argv(sys.argv[1:])


if __name__ == "__main__":
    main()
