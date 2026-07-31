#!/usr/bin/env bash
set -euo pipefail
set +x

usage() {
  echo "用法: $0 <用户名> <密码> <Topic>" >&2
  echo "示例: $0 alice 'strong-password' alice-cc-monitor" >&2
}

if [[ $# -ne 3 ]]; then
  usage
  exit 2
fi

username=$1
password=$2
container=${NTFY_CONTAINER:-ntfy}
topic=$3

if [[ ! $username =~ ^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$ ]]; then
  echo "错误: 用户名只能包含字母、数字、点、下划线和连字符，长度为 1-64。" >&2
  exit 2
fi

if [[ -z $password ]]; then
  echo "错误: 密码不能为空。" >&2
  exit 2
fi

if [[ ! $topic =~ ^[A-Za-z0-9_-]{1,64}$ ]]; then
  echo "错误: Topic 只能包含字母、数字、下划线和连字符，长度为 1-64。" >&2
  exit 2
fi

if ! command -v docker >/dev/null 2>&1; then
  echo "错误: 未找到 docker 命令。" >&2
  exit 1
fi

if ! container_state=$(
  docker inspect \
    --format '{{if .State.Running}}{{if .State.Health}}{{.State.Health.Status}}{{else}}running{{end}}{{else}}stopped{{end}}' \
    "$container" 2>/dev/null
); then
  echo "错误: ntfy 容器 '$container' 不存在或当前用户无权访问。" >&2
  exit 1
fi

case "$container_state" in
  running | healthy) ;;
  starting)
    echo "错误: ntfy 容器 '$container' 的健康检查仍在启动，请稍后重试。" >&2
    exit 1
    ;;
  unhealthy)
    echo "错误: ntfy 容器 '$container' 的健康检查未通过。" >&2
    exit 1
    ;;
  *)
    echo "错误: ntfy 容器 '$container' 未运行（状态: $container_state）。" >&2
    exit 1
    ;;
esac

echo "正在创建 ntfy 用户: $username"
docker exec \
  -e NTFY_PASSWORD="$password" \
  "$container" \
  ntfy user add "$username" --password-from-env

rollback_user() {
  echo "授权失败，正在回滚用户: $username" >&2
  docker exec "$container" ntfy user del "$username" >/dev/null 2>&1 || true
}
trap rollback_user ERR

docker exec "$container" \
  ntfy access -- "$username" "$topic" read-write

trap - ERR

echo "创建成功"
echo "用户名: $username"
echo "Topic:  $topic"
echo "权限:   read-write"
