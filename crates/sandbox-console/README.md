# sandbox-console

The web console's HTTP server: one same-origin surface the browser talks to
for the operations plane, app preview, manager-owned sandbox readiness, and the
SPA assets.
It is a **client peer** of the three `sandbox-cli` binaries, built on
`sandbox_operation_client::GatewayClient`. Semantic operation declarations and
routes come from the three enabled domains of `sandbox-operation-catalog`; the
console owns only its HTTP/JSON projection.

Boundary law: validate `/api/rpc` against public catalog routes only, never
define operation vocabulary, never contact the daemon RPC endpoint directly,
never expose the gateway auth token to the browser, and never depend on
protocol, applications, CLI, or MCP.

## Routes

```text
POST /api/rpc                                  single-operation dispatch
POST /api/rpc   (Accept: text/event-stream)    same, streaming _stream_logs as SSE
GET  /api/catalog                              management+runtime+observability catalogs
GET  /api/sandboxes/<id>/health                manager-owned readiness lookup
POST /api/sandboxes/<id>/files/list            daemon_http directory listing
ANY  /s/<id>/shared/<port>/...                 preview proxy (prefix swap to /forward)
ANY  /s/<id>/isolated=<ws-id>/<port>/...       preview proxy, isolated workspace
GET  /*                                        SPA assets + client-route fallback
```

Protocol errors return in the body with HTTP 200; transport failures map to
400 (bad body) / 502 (gateway or daemon unreachable) / 504 (timeout).
Preview requests stream bodies, tunnel WebSocket/HTTP upgrades, append
`X-Forwarded-For`, and resolve `daemon_http` endpoints through a 3s cache.

## Running

One-command bootstrap (gateway → SPA build → console, in token order):

```sh
bin/start-sandbox-console-stack             # then open http://127.0.0.1:7880
bin/start-sandbox-console-stack --skip-gateway   # keep the running gateway
```

Or piece by piece:

```sh
cargo run -p xtask -- package-console       # build SPA into dist/console
bin/start-sandbox-console                   # reads ~/.ephemeral-sandbox/gateway.token
# or explicitly:
cargo run -p sandbox-console -- \
  --bind 127.0.0.1:7880 \
  --gateway-socket 127.0.0.1:7878 \
  --gateway-auth-token TOKEN \
  --assets dist/console
```

The gateway creates that token once with private permissions and reuses it
across restarts. All launchers resolve the path through
`bin/sandbox-gateway-token`; set `SANDBOX_GATEWAY_TOKEN_FILE` when a service
install needs a system-managed path.

Config discovery: `--config-yaml` outranks `SANDBOX_CONSOLE_CONFIG_YAML` when
selecting an optional YAML `console` section. Console values use flag > env
when one exists > YAML > default precedence; assets use `--assets` >
`SANDBOX_CONSOLE_ASSETS` > the detected `dist/console` or `web/console/dist`
directory. Gateway discovery is independent and uses
`--gateway-socket`/`--gateway-auth-token` > `SANDBOX_GATEWAY_SOCKET`/
`SANDBOX_GATEWAY_AUTH_TOKEN` > its defaults. The default console bind is
`127.0.0.1:7880` and the default gateway is `127.0.0.1:7878`. Bind stays
loopback in v0 — browser auth is out of scope, matching the gateway and
`daemon_http` posture.

SPA development: `cd web/console && npm run dev` proxies `/api` and `/s`
to the running console.
