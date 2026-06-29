#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
setup.py —— 用 py2app 把 cc_monitor.py 打包成 macOS .app

用法(在 mac 上):
    python3 setup.py py2app
产物:  dist/CCMonitor.app   ← 双击即可常驻菜单栏
"""
from setuptools import setup

APP = ["cc_monitor.py"]

OPTIONS = {
    "argv_emulation": False,
    "iconfile": "assets/AppIcon.icns",   # ← .app 应用图标
    "plist": {
        "CFBundleName": "CCMonitor",
        "CFBundleDisplayName": "Claude Code Monitor",
        "CFBundleIdentifier": "com.lixinyu.ccmonitor",
        "CFBundleVersion": "2.0.0",
        "CFBundleShortVersionString": "2.0.0",
        # LSUIElement=1 → 纯菜单栏 App,不在 Dock 显示、不抢焦点
        "LSUIElement": True,
        "NSHumanReadableCopyright": "Personal tool",
    },
    "packages": ["rumps"],
    "includes": ["cc_pricing"],
    "resources": ["assets/menubar_color.png", "assets/menubar_color@2x.png", "prices.builtin.json"],
    # cc_hook.py 是被 Claude Code 单独调用的,不打进 app;随仓库分发即可
}

setup(
    app=APP,
    name="CCMonitor",
    options={"py2app": OPTIONS},
    setup_requires=["py2app"],
)
