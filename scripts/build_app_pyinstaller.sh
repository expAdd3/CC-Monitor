#!/usr/bin/env bash
# build_app_pyinstaller.sh —— 用 PyInstaller 打包(py2app 失败时用这个)
set -e
cd "$(dirname "$0")/.."  # 始终以项目根目录为工作目录

echo "==> [1/4] 选择一个【非 conda】的 python(关键!)"
# conda 的 python 会导致打包出的 .app 缺 libffi.8.dylib 而崩溃。
is_good_py() {
  local p="$1"
  [ -x "$p" ] || return 1
  "$p" - <<'PY'
import sys
prefix = sys.prefix.lower()
bad = ("conda" in prefix) or ("anaconda" in prefix) or ("miniconda" in prefix) \
      or ("continuum" in sys.version.lower())
sys.exit(1 if bad else 0)
PY
}
CANDS=(
  /opt/homebrew/bin/python3
  /usr/local/bin/python3
  /Library/Frameworks/Python.framework/Versions/Current/bin/python3
  /usr/bin/python3
)
for v in 3.14 3.13 3.12 3.11; do
  CANDS+=("/opt/homebrew/bin/python$v" "/usr/local/bin/python$v")
done
if command -v brew >/dev/null 2>&1; then
  for f in "$(brew --prefix 2>/dev/null)"/bin/python3*; do
    [ -x "$f" ] && CANDS+=("$f")
  done
fi
PYBIN=""
for cand in "${CANDS[@]}"; do
  if is_good_py "$cand"; then PYBIN="$cand"; break; fi
done
if [ -z "$PYBIN" ]; then
  echo "❌ 没找到非 conda 的 python。已检查: ${CANDS[*]}"
  echo "   解决:  brew install python ;或  PYBIN_OVERRIDE=/路径/python3 ./build_app_pyinstaller.sh"
  exit 1
fi
[ -n "${PYBIN_OVERRIDE:-}" ] && PYBIN="$PYBIN_OVERRIDE"
echo "   使用: $PYBIN"
"$PYBIN" --version

echo "==> [2/4] 虚拟环境"
rm -rf .venv
"$PYBIN" -m venv .venv
source .venv/bin/activate
pip install --upgrade pip

echo "==> [3/4] 安装 rumps + pyinstaller + Pillow"
pip install rumps pyinstaller Pillow

echo "==> [3.5/4] 生成图标(若缺失)"
if [ ! -f assets/AppIcon.icns ] && [ -f assets/icon_src.jpeg ]; then
  python scripts/make_icons.py assets/icon_src.jpeg
fi

echo "==> [4/4] 打包"
rm -rf build dist
pyinstaller CCMonitor.spec

echo ""
echo "✅ 完成!产物:  dist/CCMonitor.app"
echo "   若双击闪退,先用终端看真实报错:"
echo "     dist/CCMonitor.app/Contents/MacOS/CCMonitor"
