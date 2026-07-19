# ntfy 服务端部署与 CC-Monitor 配置

CC-Monitor 可以把 `DONE` 和 `NEEDS_INPUT` 状态异步推送到自建 ntfy。配置文件
位于 `~/.cc-monitor/notify.json`，其中可能包含密码，请勿提交到代码仓库。

## 服务端

最小 Docker Compose 配置：

```yaml
services:
  ntfy:
    image: binwiederhier/ntfy:latest
    container_name: ntfy
    restart: unless-stopped
    ports:
      - "8088:80"
    volumes:
      - ./data:/var/cache/ntfy
      - ./server.yml:/etc/ntfy/server.yml:ro
      - ./auth.db:/etc/ntfy/auth.db
    command: serve
```

`server.yml`：

```yaml
listen-http: ":80"
auth-file: /etc/ntfy/auth.db
auth-default-access: deny
cache-file: /var/cache/ntfy/cache.db
```

首次启动及创建用户：

```bash
mkdir -p data
touch auth.db
docker compose up -d
docker exec -e NTFY_PASSWORD="your-password" ntfy \
  ntfy user add USERNAME --password-from-env
docker exec ntfy ntfy access USERNAME "USERNAME-cc-monitor" read-write
```

也可以使用仓库提供的一键脚本：

```bash
chmod +x deploy/create-user.sh
./deploy/create-user.sh USERNAME 'PASSWORD'
```

如果容器名称不是默认的 `ntfy`：

```bash
NTFY_CONTAINER=my-ntfy ./deploy/create-user.sh USERNAME 'PASSWORD'
```

脚本只会授权精确的 `{username}-cc-monitor` Topic。密码通过命令行参数传入，
可能保留在 Shell 历史中；公共或多人管理的服务器建议执行后清理相关历史记录。

确保防火墙或云安全组开放服务端口。CC-Monitor 同时支持 HTTP 和 HTTPS，
可直接填写自建 ntfy 的访问地址。Topic 名称可被猜到，用户访问隔离依赖
`auth-default-access: deny` 和精确 Topic 授权。

## CC-Monitor 客户端

打开菜单栏中的 **设置 → 远程通知**，填写服务器地址、用户名、密码和优先级。
Topic 固定由用户名生成为 `{username}-cc-monitor`，无需手动
填写。先点“测试发送”，成功后点“保存配置”并启用远程推送。设置窗口直接
读写 JSON 文件，不使用 SQLite 的 `app_settings` 表。

手机端安装 ntfy 后，添加同一个自托管服务器，使用管理员分配的用户名
和密码登录，再订阅 `{username}-cc-monitor`。请同时确认系统已授予 ntfy
通知权限。

当前设置窗口编辑第一个 ntfy 后端。若配置文件中还有额外后端，保存时会原样
保留，发送时会依次尝试全部后端。

对应 Schema：

```json
{
  "enabled": true,
  "hostname": "",
  "priority_mapping": {
    "DONE": "default",
    "NEEDS_INPUT": "urgent"
  },
  "backends": [
    {
      "type": "ntfy",
      "server": "https://ntfy.example.com",
      "topic": "USERNAME-cc-monitor",
      "username": "USERNAME",
      "password": "PASSWORD"
    }
  ]
}
```
