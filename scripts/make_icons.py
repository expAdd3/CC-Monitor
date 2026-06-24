#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
make_icons.py —— 生成 macOS 图标
  1. AppIcon.icns        给 .app 用的应用图标(py2app 打包进去)
  2. menubar_icon.png    菜单栏模板图(纯黑+透明,自适应深/浅色菜单栏)
     + @2x 高清版,用于 Retina 屏

依赖: pip3 install Pillow
用法: python3 make_icons.py icon_src.jpeg
"""
import sys, os
from PIL import Image, ImageDraw

SRC = sys.argv[1] if len(sys.argv) > 1 else "icon_src.jpeg"

# ---------- 1. 圆角处理 + 生成 .icns ----------
def rounded(im, radius_ratio=0.225):
    im = im.convert("RGBA")
    w, h = im.size
    r = int(w * radius_ratio)
    mask = Image.new("L", (w, h), 0)
    d = ImageDraw.Draw(mask)
    d.rounded_rectangle([0, 0, w, h], radius=r, fill=255)
    out = Image.new("RGBA", (w, h), (0, 0, 0, 0))
    out.paste(im, (0, 0), mask)
    return out

src = Image.open(SRC).convert("RGBA")
# 缩放到 1024 标准
src = src.resize((1024, 1024), Image.LANCZOS)
app_icon = rounded(src)

# Pillow 直接存 icns(包含多分辨率)
sizes = [16, 32, 64, 128, 256, 512, 1024]
app_icon.save("assets/AppIcon.icns", format="ICNS",
              append_images=[app_icon.resize((s, s), Image.LANCZOS) for s in sizes])
# 同时存一张 png 预览
app_icon.resize((512, 512), Image.LANCZOS).save("assets/AppIcon_preview.png")
print("✅ AppIcon.icns 生成")

# ---------- 2. 菜单栏模板图 ----------
# 菜单栏图标规范:约 18x18(@2x 36x36),纯黑前景+透明背景,
# 系统会按深/浅色模式自动反色。这里画一个简洁的"雷达脉冲"符号。
def make_menubar(size):
    img = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    cx = cy = size / 2
    black = (0, 0, 0, 255)
    lw = max(1, int(size * 0.06))
    # 三段同心弧(脉冲感)
    for i, rr in enumerate([0.42, 0.30, 0.18]):
        r = size * rr
        d.arc([cx - r, cy - r, cx + r, cy + r],
              start=-60, end=60, fill=black, width=lw)
    # 中心圆点
    dot = size * 0.07
    d.ellipse([cx - dot, cy - dot, cx + dot, cy + dot], fill=black)
    return img

make_menubar(18).save("assets/menubar_icon.png")
make_menubar(36).save("assets/menubar_icon@2x.png")
print("✅ menubar_icon.png / @2x 生成")
