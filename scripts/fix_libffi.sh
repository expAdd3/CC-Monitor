#!/usr/bin/env bash
# fix_libffi.sh —— 修复 .app 缺失 libffi.8.dylib 导致的启动崩溃
#
# 原因:conda 的 _ctypes.so 依赖 @rpath/libffi.8.dylib,但打包器没收集它。
# 本脚本:在系统里找到 libffi.8.dylib → 拷进 .app/Contents/Frameworks/
#         → 用 install_name_tool 把 _ctypes.so 的依赖改成 @loader_path 指向它
#         → 重新 ad-hoc 签名。
#
# 用法:  ./fix_libffi.sh dist/CCMonitor.app
set -e

APP="${1:-dist/CCMonitor.app}"
[ -d "$APP" ] || { echo "❌ 找不到 $APP"; exit 1; }

echo "==> 1. 在系统里寻找 libffi.8.dylib"
CANDIDATES=()
# conda 环境里几乎一定有
[ -n "$CONDA_PREFIX" ] && CANDIDATES+=("$CONDA_PREFIX/lib/libffi.8.dylib")
CANDIDATES+=(
  /opt/homebrew/opt/libffi/lib/libffi.8.dylib
  /usr/local/opt/libffi/lib/libffi.8.dylib
  /opt/anaconda3/lib/libffi.8.dylib
  "$HOME/anaconda3/lib/libffi.8.dylib"
  "$HOME/miniconda3/lib/libffi.8.dylib"
  /opt/miniconda3/lib/libffi.8.dylib
)
LIBFFI=""
for c in "${CANDIDATES[@]}"; do
  [ -f "$c" ] && { LIBFFI="$c"; break; }
done
# 兜底:全盘搜一个
if [ -z "$LIBFFI" ]; then
  LIBFFI=$(find /opt /usr/local "$HOME" -name "libffi.8.dylib" 2>/dev/null | head -1 || true)
fi
[ -n "$LIBFFI" ] || { echo "❌ 系统里没找到 libffi.8.dylib,请先 brew install libffi"; exit 1; }
echo "   找到: $LIBFFI"

echo "==> 2. 拷进 .app 的 Frameworks 目录"
FW="$APP/Contents/Frameworks"
mkdir -p "$FW"
cp -f "$LIBFFI" "$FW/libffi.8.dylib"
chmod 644 "$FW/libffi.8.dylib"

echo "==> 3. 修正 _ctypes.so 对 libffi 的引用路径"
# 找到 .app 里的 _ctypes.so(py2app/pyinstaller 路径不同,统一用 find)
CTYPES=$(find "$APP" -name "_ctypes*.so" | head -1)
[ -n "$CTYPES" ] || { echo "❌ .app 里找不到 _ctypes.so"; exit 1; }
echo "   _ctypes: $CTYPES"
# 当前它依赖的 libffi 路径(可能是 @rpath/libffi.8.dylib)
OLD=$(otool -L "$CTYPES" | grep -o '[^[:space:]]*libffi.8.dylib' | head -1 || true)
echo "   原依赖: ${OLD:-(none)}"
# 计算从 _ctypes.so 到 Frameworks 的相对路径深度,统一用绝对 @rpath 不如直接绝对路径稳:
# 这里直接改成指向 .app 内 Frameworks 的相对 @loader_path
REL=$(python3 - "$CTYPES" "$FW/libffi.8.dylib" <<'PY'
import os,sys
ctypes=os.path.dirname(os.path.abspath(sys.argv[1]))
target=os.path.abspath(sys.argv[2])
print("@loader_path/"+os.path.relpath(target, ctypes))
PY
)
echo "   新依赖: $REL"
if [ -n "$OLD" ]; then
  install_name_tool -change "$OLD" "$REL" "$CTYPES"
else
  install_name_tool -add_rpath "$FW" "$CTYPES" 2>/dev/null || true
fi

echo "==> 4. 重新 ad-hoc 签名(否则改过的 .so 会被 Gatekeeper 拒绝)"
codesign --force --sign - "$FW/libffi.8.dylib" 2>/dev/null || true
codesign --force --sign - "$CTYPES" 2>/dev/null || true
codesign --force --deep --sign - "$APP" 2>/dev/null || true

echo ""
echo "✅ 修复完成。验证:"
echo "   $APP/Contents/MacOS/$(basename "$APP" .app)"