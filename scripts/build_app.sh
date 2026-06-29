#!/usr/bin/env bash
# build_app.sh —— 在 mac 上一键打包成 .app(零基础也能跑)
set -e
cd "$(dirname "$0")/.."  # 始终以项目根目录为工作目录

echo "==> [1/4] 选择一个【非 conda】的 python(关键!)"
# py2app + conda 会导致 libffi.8.dylib 缺失、.app 启动即崩,
# 因此优先用 Homebrew / python.org 的 python。

# 判断某个 python 是不是 conda:返回 0=非conda(可用),1=是conda/不可用
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

# 候选:固定路径 + brew 带版本号的名字(python3.14 / 3.13 / 3.12 ...)
CANDS=(
  /opt/homebrew/bin/python3
  /usr/local/bin/python3
  /Library/Frameworks/Python.framework/Versions/Current/bin/python3
  /usr/bin/python3
)
for v in 3.14 3.13 3.12 3.11; do
  CANDS+=("/opt/homebrew/bin/python$v" "/usr/local/bin/python$v")
done
# 把 brew --prefix 下的也加进来(更稳)
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
  echo "❌ 没找到非 conda 的 python。"
  echo "   已检查: ${CANDS[*]}"
  echo "   解决:  brew install python  然后重试;"
  echo "   或手动指定:  PYBIN=/路径/python3 ./build_app.sh"
  exit 1
fi
# 允许用户用环境变量强制覆盖
[ -n "${PYBIN_OVERRIDE:-}" ] && PYBIN="$PYBIN_OVERRIDE"
echo "   使用: $PYBIN"
"$PYBIN" --version

echo "==> [2/4] 创建独立虚拟环境(基于上面选定的 python)"
rm -rf .venv
"$PYBIN" -m venv .venv
source .venv/bin/activate

echo "==> [3/4] 安装依赖 rumps + py2app + Pillow"
pip install --upgrade pip
pip install rumps py2app Pillow

echo "==> [3.5/4] 生成图标(若缺失)"
if [ ! -f assets/AppIcon.icns ] && [ -f assets/icon_src.jpeg ]; then
  python scripts/make_icons.py assets/icon_src.jpeg
fi

echo "==> [4/4] 打包"
rm -rf build

BASE_APP="dist/CCMonitor.app"
OUT_APP="$BASE_APP"
if [ -e "$BASE_APP" ]; then
  i=2
  while [ -e "dist/CCMonitor-v${i}.app" ]; do
    i=$((i + 1))
  done
  OUT_APP="dist/CCMonitor-v${i}.app"
  echo "   检测到已存在 dist/CCMonitor.app，新产物将输出为: $OUT_APP"
fi

TMP_DIST=".dist-tmp"
rm -rf "$TMP_DIST"
python setup.py py2app --dist-dir "$TMP_DIST"

mkdir -p dist
mv "$TMP_DIST/CCMonitor.app" "$OUT_APP"
rm -rf "$TMP_DIST"

echo "==> [收尾] 修复可能缺失的 libffi(conda 环境保险)"
[ -f scripts/fix_libffi.sh ] && bash scripts/fix_libffi.sh "$OUT_APP" || true

echo ""
echo "✅ 完成!产物在:  $OUT_APP"
echo "   把它拖进 /Applications,双击即可常驻菜单栏。"
echo ""
echo "别忘了注册 hook(让监控变准):"
echo "   python3 install_hooks.py"
