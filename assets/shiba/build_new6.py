#!/usr/bin/env python3
"""PetPom 动画资产管线 · V1 遗留入口（build_new6.py 兼容，2026-09-22 重建）

历史作用：V1 时代「6 动画 × 10 帧」新资产管线（pet/sad 重做等）。
已被 build_v2.py 的 --legacy-v1 模式完整覆盖（176×150 · 26 色 · 身高锚点 96/80）。

用法（与历史命令等价）：
  .venv/bin/python assets/shiba/build_new6.py --src 源帧目录/ --out ui/assets/petpacks/tuantuan/anims
"""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import build_v2  # noqa: E402


def main() -> None:
    # 把 --legacy-v1 注入参数，其余原样透传
    argv = sys.argv[1:]
    if "--legacy-v1" not in argv:
        argv = ["--legacy-v1"] + argv
    build_v2.main_with_argv(argv)


if __name__ == "__main__":
    main()
