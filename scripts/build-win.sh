#!/usr/bin/env bash
#
# 一键构建 Windows 版 RustMobaX
#
# 流程：本地打包源码 -> 上传构建机 -> 覆盖解包 -> 安装前端依赖 -> 交叉编译 NSIS 包。
# 只负责同步与构建，不回传产物，编译结果留在构建机上。
#
# 可用环境变量覆盖：
#   RMBX_HOST    构建机地址，默认 172.25.103.134
#   RMBX_USER    构建机用户，默认 huya
#   RMBX_PASS    构建机密码，必填，不要把密码写进脚本或提交到仓库
#   RMBX_DIR     构建机工作目录，默认 /home/huya/work/RustMobaX-win
#   RMBX_TARGET  Rust 目标三元组，默认 x86_64-pc-windows-gnu
#
# 用法：bash scripts/build-win.sh
#
# 说明：解包采用覆盖方式，不清空工作目录，这样构建机上的 node_modules 与
# target 缓存可以复用。已经删除的源文件不会自动消失，偶尔需要手动清理。

set -euo pipefail

REMOTE_HOST="${RMBX_HOST:-172.25.103.134}"
REMOTE_USER="${RMBX_USER:-huya}"
REMOTE_PASS="${RMBX_PASS:?请用环境变量 RMBX_PASS 指定构建机密码}"
REMOTE_DIR="${RMBX_DIR:-/home/huya/work/RustMobaX-win}"
TARGET="${RMBX_TARGET:-x86_64-pc-windows-gnu}"

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO_DIR=$(dirname "$SCRIPT_DIR")
REPO_NAME=$(basename "$REPO_DIR")
PARENT_DIR=$(dirname "$REPO_DIR")

# 环境变量名里的连字符要换成下划线，例如 x86_64-pc-windows-gnu -> x86_64_pc_windows_gnu
TARGET_ENV="${TARGET//-/_}"
LINKER_ENV="CARGO_TARGET_${TARGET_ENV^^}_LINKER"

SSH_OPTS=(-o StrictHostKeyChecking=no -o ServerAliveInterval=30 -o ServerAliveCountMax=6)
SCP_OPTS=(-o StrictHostKeyChecking=no)

command -v sshpass >/dev/null 2>&1 || {
  echo "缺少 sshpass，请先安装后再运行本脚本" >&2
  exit 1
}
command -v tar >/dev/null 2>&1 || {
  echo "缺少 tar" >&2
  exit 1
}

remote() {
  sshpass -p "$REMOTE_PASS" ssh "${SSH_OPTS[@]}" "$REMOTE_USER@$REMOTE_HOST" "$@"
}

ARCHIVE=$(mktemp /tmp/rmx-win-src.XXXXXX.tgz)
trap 'rm -f "$ARCHIVE"' EXIT

echo "[1/5] 打包源码：$REPO_NAME"
tar czf "$ARCHIVE" -C "$PARENT_DIR" \
  --exclude="$REPO_NAME/node_modules" \
  --exclude="$REPO_NAME/target" \
  --exclude="$REPO_NAME/dist" \
  --exclude="$REPO_NAME/.git" \
  "$REPO_NAME"
echo "      包大小 $(stat -c '%s' "$ARCHIVE") 字节"

echo "[2/5] 上传到 $REMOTE_USER@$REMOTE_HOST"
sshpass -p "$REMOTE_PASS" scp "${SCP_OPTS[@]}" "$ARCHIVE" "$REMOTE_USER@$REMOTE_HOST:/tmp/rmx-win-src.tgz"

echo "[3/5] 覆盖解包到 $REMOTE_DIR"
remote "mkdir -p '$REMOTE_DIR' && tar xzf /tmp/rmx-win-src.tgz -C '$REMOTE_DIR' --strip-components=1 && rm -f /tmp/rmx-win-src.tgz"

echo "[4/5] 安装前端依赖"
remote "cd '$REMOTE_DIR' && bun install --frozen-lockfile"

echo "[5/5] 交叉编译到 $TARGET（首次全量约 10 分钟，增量约 3 到 5 分钟）"
remote "set -o pipefail; cd '$REMOTE_DIR' && \
  export CC_${TARGET_ENV}=x86_64-w64-mingw32-gcc && \
  export CXX_${TARGET_ENV}=x86_64-w64-mingw32-g++ && \
  export AR_${TARGET_ENV}=x86_64-w64-mingw32-ar && \
  export ${LINKER_ENV}=x86_64-w64-mingw32-gcc && \
  bun run tauri build --target '$TARGET' --bundles nsis 2>&1 | tee /tmp/rmx-win-build.log | tail -n 20"

echo ""
echo "构建完成，产物在构建机 $REMOTE_DIR："
remote "stat -c '%n  %s 字节  %y' \
  '$REMOTE_DIR/target/$TARGET/release/rustmobax.exe' \
  '$REMOTE_DIR/target/$TARGET/release/bundle/nsis/RustMobaX_0.1.0_x64-setup.exe'"

echo ""
echo "完整构建日志：构建机 /tmp/rmx-win-build.log"
