#!/usr/bin/env bash
set -euo pipefail
set +x

usage() {
  echo "用法: $0 <用户名> <密码>" >&2
  echo "示例: $0 alice 'strong-password'" >&2
}

if [[ $# -ne 2 ]]; then
  usage
  exit 2
fi

username=$1
password=$2
container=${NTFY_CONTAINER:-ntfy}
topic="${username}-cc-monitor"

if [[ ! $username =~ ^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$ ]]; then
  echo "错误: 用户名只能包含字母、数字、点、下划线和连字符，长度为 1-64。" >&2
  exit 2
fi

if [[ -z $password ]]; then
  echo "错误: 密码不能为空。" >&2
  exit 2
fi

if ! command -v docker >/dev/null 2>&1; then
  echo "错误: 未找到 docker 命令。" >&2
  exit 1
fi

if ! docker inspect "$container" >/dev/null 2>&1; then
  echo "错误: ntfy 容器 '$container' 不存在或当前用户无权访问。" >&2
  exit 1
fi

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
  ntfy access "$username" "$topic" read-write

trap - ERR

echo "创建成功"
echo "用户名: $username"
echo "Topic:  $topic"
echo "权限:   read-write"
