# -*- mode: python ; coding: utf-8 -*-
# CCMonitor.spec —— PyInstaller 打包配置(py2app 的备选方案)
# 用法:  pyinstaller CCMonitor.spec
# 产物:  dist/CCMonitor.app

block_cipher = None

a = Analysis(
    ['cc_monitor.py'],
    pathex=[],
    binaries=[],
    datas=[
        ('assets/menubar_color.png', 'assets'),
        ('assets/menubar_color@2x.png', 'assets'),
    ],
    hiddenimports=['rumps'],   # rumps 内部动态导入 pyobjc,显式声明更稳
    hookspath=[],
    runtime_hooks=[],
    excludes=[],
    cipher=block_cipher,
)
pyz = PYZ(a.pure, a.zipped_data, cipher=block_cipher)

exe = EXE(
    pyz, a.scripts, [],
    exclude_binaries=True,
    name='CCMonitor',
    debug=False, strip=False, upx=False, console=False,
)
coll = COLLECT(
    exe, a.binaries, a.zipfiles, a.datas,
    strip=False, upx=False, name='CCMonitor',
)
app = BUNDLE(
    coll,
    name='CCMonitor.app',
    icon='assets/AppIcon.icns',
    bundle_identifier='com.ccmonitor.app',
    info_plist={
        'LSUIElement': True,          # 纯菜单栏,不进 Dock、不抢焦点
        'CFBundleName': 'CCMonitor',
        'CFBundleDisplayName': 'Claude Code Monitor',
        'CFBundleShortVersionString': '2.0.0',
    },
)
