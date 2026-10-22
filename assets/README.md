# PetPom 资产管线（2026-09-22 重建）

> 背景：动画资产规格见 `docs/animation-spec.md`。此前的管线脚本（`build_v2.py` / `build_new6.py`）
> 未入库，导致资产无法复现。本目录重建了可运行的管线脚本，并记录缺失的上游素材。

## 目录

| 文件 | 说明 |
|---|---|
| `shiba/build_v2.py` | **V2 现行管线**：抹水印 → 抠底 → 去阴影/碎块 → 面积归一 → 40 色共享调色板量化(int32) → 192×192 画布贴底居中；含 `--legacy-v1` 兼容模式 |
| `shiba/build_new6.py` | V1 遗留入口（薄封装，等价于 `build_v2.py --legacy-v1`） |
| `requirements.txt` | 依赖：pillow / numpy |
| `.venv/` | 项目本地虚拟环境（不入库） |

## 安装

```bash
python3 -m venv assets/.venv
assets/.venv/bin/pip install -r assets/requirements.txt
```

## 用法

```bash
# V2 标准（柴犬等新形象包）
assets/.venv/bin/python assets/shiba/build_v2.py \
    --src <源帧目录> --out ui/assets/petpacks/<pack>/anims \
    [--anchor rest-stretch=max,sleep=max] [--pick-loop 6] [--force]

# 第 0 步：视频 → 帧目录（源素材是「图生视频」产出的 mp4 时）
assets/.venv/bin/python assets/extract_frames.py \
    --pack tuantuan --video-root assets/tuantuan/video --out /tmp/frames
# 单动画也行：--src <含 mp4 的目录> --out <输出根> --frames 6 --pick loop|span|blink

# V1 遗留（团团等旧形象包，176×150 · 26 色 · 身高锚点 96/80）
assets/.venv/bin/python assets/shiba/build_new6.py \
    --src <源帧目录> --out ui/assets/petpacks/tuantuan/anims

# 校验循环动画首尾帧差（红线 < 5/255）
assets/.venv/bin/python assets/shiba/build_v2.py --src ... --out ... --check-loop
```

输入目录约定：`<src>/<anim>/<anim>-NN.png`（如 `<src>/walk/walk-01.png`）。
输出：`<out>/<anim>/<anim>-NN.png`（同名文件已存在时跳过，`--force` 覆盖）。

## 管线步骤（与 animation-spec.md §3/§4 对齐）

1. **抹水印 / 抠底**：从画布四边 floodFill(FIXED_RANGE, tol=60) 标记近白背景并置透明；
   贴边角水印因与背景连通被一并抹除。仅当边缘主色为近白（RGB 和 ≥680、极差 ≤50）且覆盖
   边缘 ≥50% 时才执行——对已抠底的干净帧（透明底）与贴边精灵自动跳过。
2. **alpha 二值化**：`>140 → 255`，其余 0。
3. **去阴影**：透明区膨胀 1 像素，落在带内且亮度 <46 的像素转透明。
4. **去碎块**：8-连通分量面积 <6 的孤立点清除。
5. **面积归一（V2）**：同动画共用一个缩放因子 `scale = √(target/ref)`，ref 取帧面积
   **max（伸展/趴卧类）或 median（其余）**——用中位数会把坐姿帧放大 26%（rest-stretch 踩坑）。
   V1 遗留模式为**身高锚点**：每帧身高归一至 96（站立/坐姿）或 80（趴卧/蜷睡）。
6. **共享调色板量化**：跨全部帧采样 k-means（k=40，k-means++ 初始化），逐像素映射最近色；
   **距离平方必须 int32**——int16 下 255²=65025 溢出为负，argmin 选错色板项（2026-09-15 踩坑）。
7. **画布贴底居中**：192×192（V2）/ 176×150（V1）RGBA，脚底贴画布底边、质心水平居中。
8. **循环质量**：`--check-loop` 校验首尾帧不透明区平均绝对差（<5/255 合格）；
   `--pick-loop N` 从长序列挑首尾帧差最小的连续 N 帧。

## ⚠️ 缺失的上游素材（无法在仓库内复现的部分）

- **源视频 / 基准图**：V2 管线第一步「图生视频」（同一基准图驱动）与 V1 的「文生视频」
  属于生成环节，源视频与基准图不在仓库。现有精灵图可直接作为 `--src` 重新过一遍管线
  （近恒等），但**从零重建原始帧需要重新跑生成**。
- 原始生成参数（prompt、seed、模型）未入库。若需完整复现资产，请补充基准图与生成记录后
  用本管线重跑。

## 验证记录（2026-09-22）

- `assets/.venv/bin/python assets/shiba/build_v2.py --src ui/assets/petpacks/shiba/anims --out /tmp/petpom-pipe-check`：
  对现有 16 组柴犬动画全量重跑成功，输出 192×192 RGBA，与现网资产视觉一致（见该次运行输出）。
- V1 遗留模式对团团动画全量重跑成功，输出 176×150（与现网尺寸一致）。

## 改完 manifest.json 必须做的一件事

```bash
assets/.venv/bin/python assets/gen_pack_js.py
```

运行时用 `<script>` 加载 `manifest.js`（WKWebView 下 `fetch` 在自定义协议上不可用），
`manifest.json` 仍是唯一数据源，`.js` 是生成物。两个文件一起提交。

## 改完 manifest.json 必须做的一件事

```bash
assets/.venv/bin/python assets/gen_pack_js.py
```

运行时用 `<script>` 加载 `manifest.js`（WKWebView 下 `fetch` 在自定义协议上不可用），
`manifest.json` 仍是唯一数据源，`.js` 是生成物。两个文件一起提交。
