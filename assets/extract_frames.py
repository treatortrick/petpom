#!/usr/bin/env python3
"""从「图生视频」源文件抽帧 → 帧目录（供 assets/shiba/build_v2.py --src 消费）

为什么单独成一步：build_v2.py 只处理「已抽好的帧」（不需要 cv2，依赖更轻），
而视频→帧这一步必须依赖 OpenCV。拆开后两条命令各司其职，也能单独重跑抽帧。

用法：
  # 单动画
  assets/.venv/bin/python assets/extract_frames.py \
      --src assets/tuantuan/video/v2-idle --out /tmp/frames/idle \
      --frames 4 --pick loop

  # 整包批量（按内置 ANIM_MAP）
  assets/.venv/bin/python assets/extract_frames.py \
      --pack tuantuan --video-root assets/tuantuan/video --out /tmp/frames

pick 模式：
  loop  循环闭合选帧（呼吸/循环类）：找首尾最像的一对，等距取样
  span  等距取关键姿势（一次性动作）
  spread 姿势差异最大化（贪心最远点）：运动集中在某小段时用它，
        否则动画会看起来是静止的（walk / focus-read / sleep 建议用这个）
  blink 从 idle 视频里挑「睁→闭→睁」三帧
"""
import argparse
import json
import sys
from pathlib import Path

import cv2
import numpy as np
from PIL import Image

# 每个形象包的动画表：动画名 → (视频目录名, 帧数, pick 模式, 额外参数)
# 与 ui/assets/petpacks/<pack>/manifest.json 的 anims 帧数保持一致
ANIM_MAP = {
    "shiba": {
        "idle": ("v2-idle", 4, "loop", None),
        "sit": ("v2-sit", 4, "loop", None),
        "sleep": ("v2-sleep", 4, "spread", None),
        "focus-read": ("v2-focus-read", 4, "spread", None),
        "walk": ("v2-walk2", 6, "spread", None),
        "micro-blink": ("v2-idle", 3, "blink", None),
        "micro-ear": ("v2-micro-ear", 3, "span", None),
        "micro-tilt": ("v2-micro-tilt", 3, "span", None),
        "pet": ("v2-pet", 4, "span", None),
        "happy": ("v2-happy", 6, "span", None),
        "sad": ("v2-sad", 4, "span", None),
        "alert": ("v2-alert", 6, "span", None),
        "eat": ("v2-eat", 6, "span", None),
        "focus-tidy": ("v2-focus-tidy", 6, "span", None),
        "rest-stretch": ("v2-rest-stretch", 6, "span", None),
        "focus-nudge": ("v2-focus-nudge", 4, "span", None),
    },
    "tuantuan": {
        "idle": ("v2-idle", 4, "loop", None),
        "sit": ("v2-sit", 4, "loop", None),
        "sleep": ("v2-sleep", 4, "spread", None),
        "focus-read": ("v2-focus-read", 4, "spread", None),
        "walk": ("v2-walk", 6, "spread", None),
        "micro-blink": ("v2-idle", 3, "blink", None),
        "micro-ear": ("v2-micro-ear", 3, "span", None),
        "micro-tilt": ("v2-micro-tilt", 3, "span", None),
        "pet": ("v2-pet", 4, "span", None),
        "happy": ("v2-happy", 6, "span", None),
        "sad": ("v2-sad", 4, "span", None),
        "alert": ("v2-alert", 6, "span", None),
        "eat": ("v2-eat", 6, "span", None),
        "focus-tidy": ("v2-focus-tidy", 6, "span", None),
        "rest-stretch": ("v2-rest-stretch", 6, "span", None),
        "focus-nudge": ("v2-focus-nudge", 4, "span", None),
    },
}


def latest_mp4(d: Path):
    fs = sorted(d.glob("*.mp4"))
    return fs[-1] if fs else None


def load_video(src: Path):
    cap = cv2.VideoCapture(str(src))
    frames = []
    while True:
        ok, f = cap.read()
        if not ok:
            break
        frames.append(f[:, :, ::-1])  # BGR → RGB
    cap.release()
    if not frames:
        raise SystemExit(f"[extract] 读取失败或空视频：{src}")
    return frames


def strip_watermark(f):
    """抹掉生成器右下角固定位置的水印带"""
    h, w = f.shape[:2]
    f[int(h * 0.90):, int(w * 0.52):] = f[4, 4]
    return f


def pick_spread_in(sigs, lo, hi, n):
    """在 [lo, hi) 区间内做「差异最大化」选帧（贪心最远点）。

    为什么需要两段式：单纯最大化差异（pick_spread）会跨出动作边界——
    实测它把睡觉视频里「起床站起来」的帧也抽进来了，睡眠循环变成
    「坐着醒着 → 突然趴下」。所以必须**先框定周期窗口，再在窗口内最大化差异**。
    """
    # 循环闭合余量：不选到窗口最末尾，留出约一个「步长」，
    # 这样循环回跳（末帧→首帧）只跨越一步，而不是从最远点硬跳回起点。
    step = max(1, (hi - lo) // max(n, 1))
    hi_eff = max(lo + 1, hi - step)
    idx = list(range(lo, hi_eff))
    if len(idx) <= n:
        idx = list(range(lo, max(hi, lo + 1)))
    if len(idx) <= n:
        return idx
    start = max(idx, key=lambda i: float(np.abs(sigs[i] - sigs[idx[0]]).mean()))
    sel = [start]
    while len(sel) < n:
        best, best_d = None, -1.0
        for i in idx:
            if i in sel:
                continue
            d = min(float(np.abs(sigs[i] - sigs[j]).mean()) for j in sel)
            if d > best_d:
                best_d, best = d, i
        if best is None:
            break
        sel.append(best)
    return sorted(sel)


def pick_loop(frames, n):
    """循环选帧：在「首尾能接上」和「窗口内有真实动作」之间联合评分。

    踩过的坑（两次）：
      1. 只按闭合找窗口 → 算法天生偏好**最静止的片段**（静止段首尾差异最小），
         实测挑到了视频结尾的定格段，睡觉/看书变成静止图；
      2. 只最大化差异 → 会跨出动作边界，把「起床站立」的帧抽进睡眠循环。
    现在：score = 窗口内平均帧间差 − 0.6 × 首尾差，取最高分的窗口，再在窗口内做
    差异最大化选帧。既保证能无缝循环，又保证循环里有动作。
    """
    import numpy as np
    sigs = [
        cv2.resize(cv2.cvtColor(f, cv2.COLOR_RGB2GRAY), (32, 32)).astype(np.float32)
        for f in frames
    ]
    N = len(sigs)
    best = None
    for i0 in range(N):
        for j0 in range(i0 + 8, min(i0 + 85, N)):
            closure = float(np.abs(sigs[i0] - sigs[j0]).mean())
            seg = sigs[i0:j0 + 1]
            internal = float(np.mean([np.abs(seg[k] - seg[k - 1]).mean() for k in range(1, len(seg))]))
            score = internal - 0.6 * closure
            if best is None or score > best[0]:
                best = (score, i0, j0)
    _, a, b = best
    return pick_spread_in(sigs, a, b + 1, n)


def pick_spread(frames, n):
    """姿势差异最大化选帧（贪心最远点）。

    为什么需要它：等距取样（span）会平均地取到整段视频——如果运动只集中在
    某一小段（比如转身、抬腿那几帧），等距取样就"绕开"了运动，
    出来的动画看起来是静止的（实测：柴犬走路帧间差只有猫的 1/3，
    猫的看书动作几乎是静止图、睡觉完全不呼吸）。
    这里改成：先把每帧压成 32×32 的灰度签名，再逐步挑"与已选帧差异最大"的那帧。
    """
    import numpy as np
    sigs = [
        cv2.resize(cv2.cvtColor(f, cv2.COLOR_RGB2GRAY), (32, 32)).astype(np.float32)
        for f in frames
    ]
    lo, hi = int(len(frames) * 0.05), int(len(frames) * 0.95)
    idx = list(range(lo, max(hi, lo + 1)))
    if len(idx) <= n:
        return idx
    # 起点：相对段落开头变化最大的那一帧
    start = max(idx, key=lambda i: float(np.abs(sigs[i] - sigs[idx[0]]).mean()))
    sel = [start]
    while len(sel) < n:
        best, best_d = None, -1.0
        for i in idx:
            if i in sel:
                continue
            d = min(float(np.abs(sigs[i] - sigs[j]).mean()) for j in sel)
            if d > best_d:
                best_d, best = d, i
        if best is None:
            break
        sel.append(best)
    return sorted(sel)


def pick_span(frames, n):
    N = len(frames)
    i0, i1 = int(N * 0.08), int(N * 0.92)
    return list(np.linspace(i0, i1 - 1, n).round().astype(int))


def pick_blink(frames, n):
    """从 idle 视频里挑「睁→闭→睁」：按眼部区域明暗取极值"""
    scores = []
    for f in frames:
        g = cv2.cvtColor(f, cv2.COLOR_RGB2GRAY)
        h, w = g.shape
        eye = g[int(h * 0.30):int(h * 0.45), int(w * 0.30):int(w * 0.70)]
        scores.append(eye.mean())
    lo, hi = int(np.argmin(scores)), int(np.argmax(scores))
    open_i = hi if abs(scores[0] - scores[hi]) < abs(scores[0] - scores[lo]) else lo
    mid = lo if open_i == hi else hi
    return [open_i, mid, open_i][:n]


def extract_one(video_dir: Path, out_dir: Path, n: int, pick: str):
    src = latest_mp4(video_dir)
    if src is None:
        raise SystemExit(f"[extract] 目录下没有 mp4：{video_dir}（源目录名对不上是最常见原因）")
    frames = [strip_watermark(f) for f in load_video(src)]
    idx = {"loop": pick_loop, "span": pick_span, "blink": pick_blink, "spread": pick_spread}[pick](frames, n)
    out_dir.mkdir(parents=True, exist_ok=True)
    for old in out_dir.glob("*.png"):
        old.unlink()
    for i, k in enumerate(idx):
        k = int(min(max(k, 0), len(frames) - 1))
        Image.fromarray(frames[k]).save(out_dir / f"{out_dir.name}-{i + 1:02d}.png")
    print(f"[extract] {video_dir.name} → {out_dir}  帧 {len(idx)}  选帧 {pick}  ({src.name})")


def main():
    ap = argparse.ArgumentParser(description="视频 → 帧目录（PetPom 资产管线第一步）")
    ap.add_argument("--src", help="单个动画的视频目录（含 mp4）")
    ap.add_argument("--out", required=True, help="输出根目录")
    ap.add_argument("--frames", type=int, default=6)
    ap.add_argument("--pick", default="span", choices=["loop", "span", "blink", "spread"])
    ap.add_argument("--pack", choices=sorted(ANIM_MAP), help="批量模式：按内置 ANIM_MAP 抽整包")
    ap.add_argument("--video-root", help="批量模式的视频根目录（含 v2-* 子目录）")
    args = ap.parse_args()

    out_root = Path(args.out)
    if args.pack:
        video_root = Path(args.video_root or f"assets/{args.pack}/video")
        for anim, (vdir, n, pick, _) in ANIM_MAP[args.pack].items():
            extract_one(video_root / vdir, out_root / anim, n, pick)
        print(f"[extract] 完成：{args.pack} 共 {len(ANIM_MAP[args.pack])} 个动画 → {out_root}")
        return
    if not args.src:
        ap.error("单动画模式需要 --src")
    # 视频目录习惯叫 v2-<anim>，但帧目录要用 <anim>（build_v2.py 按 <anim>/<anim>-NN.png 读）
    name = Path(args.src).name
    if name.startswith("v2-"):
        name = name[3:]
    extract_one(Path(args.src), out_root / name, args.frames, args.pick)


if __name__ == "__main__":
    main()
