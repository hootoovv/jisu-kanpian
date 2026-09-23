#!/usr/bin/env python3
"""生成极速看片全套图标（胶片 + 绿色播放三角，黑底黄点缀）"""
from PIL import Image, ImageDraw

BG = (0, 0, 0, 255)
YELLOW = (255, 214, 10, 255)
GREEN = (63, 185, 80, 255)


def draw_icon(size: int) -> Image.Image:
    img = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    s = size / 128.0  # 以 128 为基准的缩放系数

    # 圆角黑色底板
    r = 28 * s
    d.rounded_rectangle([0, 0, size - 1, size - 1], radius=r, fill=BG)

    # 左右两条胶片带（黄色描边圆角矩形 + 齿孔）
    band_l = (10 * s, 16 * s, 30 * s, size - 16 * s)
    band_r = (size - 30 * s, 16 * s, size - 10 * s, size - 16 * s)
    for band in (band_l, band_r):
        d.rounded_rectangle(band, radius=6 * s, outline=YELLOW, width=int(3 * s) or 1)
    # 齿孔：每条带上下各 2 个
    hole_w, hole_h = 12 * s, 14 * s
    for band in (band_l, band_r):
        x0 = (band[0] + band[2]) / 2 - hole_w / 2
        ys = [band[1] + 4 * s, (band[1] + band[3]) / 2 - hole_h / 2, band[3] - 4 * s - hole_h]
        for y0 in ys:
            d.rounded_rectangle([x0, y0, x0 + hole_w, y0 + hole_h], radius=3 * s, fill=YELLOW)

    # 中央绿色播放三角（圆角）
    cx, cy = size * 0.54, size * 0.5
    w, h = 56 * s, 64 * s
    tri = [(cx - w / 2, cy - h / 2), (cx - w / 2, cy + h / 2), (cx + w / 2, cy)]
    d.polygon(tri, fill=GREEN)
    # 三角左缘补两条竖线做圆角感
    d.rectangle([cx - w / 2, cy - h / 2 + 3 * s, cx - w / 2 + 4 * s, cy + h / 2 - 3 * s], fill=GREEN)
    return img


import os

out = os.path.join(os.path.dirname(__file__), "..", "src-tauri", "icons")
os.makedirs(out, exist_ok=True)

# 常规尺寸
for size, name in [(32, "32x32.png"), (128, "128x128.png"), (256, "128x128@2x.png"), (1024, "icon.png"), (1024, "icon-source.png")]:
    draw_icon(size).save(os.path.join(out, name))

# Windows ICO（内嵌多尺寸 PNG）
icon = draw_icon(256)
icon.save(os.path.join(out, "icon.ico"), sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)])

# macOS ICNS：Python 标准库不写 icns；此文件用 tauri CLI 再生成（见 README）。
# 这里占位复制一份 1024 png 保证 bundle 配置不缺文件的说法由文档说明。
draw_icon(1024).save(os.path.join(out, "icon.icns"))  # 占位（非真 icns）
print("icons written to", out)
