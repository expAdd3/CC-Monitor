# Self-hosted ntfy setup

CC Monitor can send `Done`, `Needs Input`, and `Failed` events to an ntfy
server. Notification settings and credentials are stored in the application's
local SQLite database and are excluded from logs and copied diagnostics.

## Server

Minimal Docker Compose configuration:

```yaml
services:
  ntfy:
    image: binwiederhier/ntfy:${NTFY_VERSION:?set NTFY_VERSION to an explicit release}
    container_name: ntfy
    restart: unless-stopped
    ports:
      # Keep the plaintext upstream on the same host. Do not publish this
      # port on a LAN or public interface.
      - "127.0.0.1:8088:80"
    volumes:
      - ./data:/var/cache/ntfy
      - ./server.yml:/etc/ntfy/server.yml:ro
      - ./auth.db:/etc/ntfy/auth.db
    command: serve
```

`server.yml`:

```yaml
listen-http: ":80"
base-url: "https://ntfy.example.com"
auth-file: /etc/ntfy/auth.db
auth-default-access: deny
cache-file: /var/cache/ntfy/cache.db
```

Replace `ntfy.example.com` with the public hostname and terminate TLS on the
same host before forwarding to `127.0.0.1:8088`. For example, a Caddy site can
use:

```caddyfile
ntfy.example.com {
    reverse_proxy 127.0.0.1:8088
}
```

Only expose the HTTPS endpoint to clients. Do not enter the container's
plaintext upstream address in CC Monitor when authentication is enabled.

Start the server:

```sh
mkdir -p data
touch auth.db
export NTFY_VERSION='<reviewed-ntfy-release>'
docker compose up -d
```

Replace the placeholder with a reviewed release number. The required variable
keeps deployments reproducible and prevents an unattended pull of `latest`.
`create-user.sh` refuses to mutate a stopped container, or a container whose
configured Docker health check is still starting or unhealthy.

Create one user and grant access to one topic:

```sh
./deploy/create-user.sh USERNAME 'PASSWORD' TOPIC
```

The script is intentionally one-shot and create-only. If `USERNAME` already
exists, `ntfy user add` fails and the script does not change that user's
password or ACLs and does not delete the existing user. Choose a new username,
or manage an existing account explicitly with the ntfy administration CLI.
Only an ACL failure after a successful new-user creation triggers deletion of
that newly created user.

`TOPIC` is required and must match the value configured in CC Monitor. It is
1-64 characters long and may contain ASCII letters, digits, `_`, and `-`.
Leading `_` or `-` is valid. The provisioning script passes ACL operands after
an explicit `--` option terminator, so a leading `-` remains topic data rather
than being parsed as an ntfy option. The topic is independent of the ntfy
username. When the container is not named `ntfy`, set
`NTFY_CONTAINER`:

```sh
NTFY_CONTAINER=my-ntfy ./deploy/create-user.sh USERNAME 'PASSWORD' TOPIC
```

The password is passed as a command-line argument and may remain in shell
history. Use an appropriate secret-handling workflow on shared systems.

## CC Monitor

Open **Settings → Remote notifications** and enter:

- the full public `https://` server URL;
- the same topic granted above;
- the username and password when authentication is enabled.

Use **Test message** before saving. The test uses the currently displayed form
values without persisting them. After it succeeds, save the settings and
subscribe to the same server and topic in the ntfy mobile or desktop client.

With `auth-default-access: deny`, access remains limited to explicitly granted
users and topics.

CC Monitor accepts plaintext HTTP only for credential-free local development
servers whose host is exactly `localhost`, `127.0.0.1`, or `::1`. It rejects
HTTP for every other host and rejects usernames or passwords over HTTP,
including loopback. This exception is not a production deployment mode.
