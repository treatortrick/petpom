#!/usr/bin/env python3
"""把 manifest.json 生成为 manifest.js（供前端用 <script> 加载）

为什么需要它：WKWebView 下页面自身的自定义协议（tauri://）不支持 fetch/XHR
（2026-09-23 定位：连 fetch("index.html") 都抛 TypeError: Load failed），
但 <script src> / <img src> 这类子资源加载是正常的。
因此运行时改用脚本方式读取 manifest；manifest.json 依然是唯一数据源。

用法：
  assets/.venv/bin/python assets/gen_pack_js.py
改完任意 manifest.json 后跑一次，两个文件一起提交。
"""
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PACKS = ["shiba", "tuantuan"]


def main() -> None:
    for pack in PACKS:
        mj = ROOT / f"ui/assets/petpacks/{pack}/manifest.json"
        if not mj.exists():
            print(f"[gen] 跳过（无 manifest）: {mj}")
            continue
        data = json.loads(mj.read_text(encoding="utf-8"))
        js = ROOT / f"ui/assets/petpacks/{pack}/manifest.js"
        body = json.dumps(data, ensure_ascii=False, separators=(",", ":"))
        js.write_text(
            "// 自动生成，勿手改 —— 源：manifest.json（见 assets/gen_pack_js.py）\n"
            "window.__PETPOM_PACKS__ = window.__PETPOM_PACKS__ || {};\n"
            f"window.__PETPOM_PACKS__[{pack!r}] = {body};\n",
            encoding="utf-8",
        )
        print(f"[gen] {js.relative_to(ROOT)}  ({js.stat().st_size} 字节)")


if __name__ == "__main__":
    main()
