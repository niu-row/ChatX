#!/usr/bin/env bash
# Remote deployment: root/passwordless sudo only, existing HTTPS site required.
set -Eeuo pipefail
mode="${1:-}"
domain="${2:-}"
desktop="${3:-}"
archive="${4:-}"
if [[ ! "$domain" =~ ^[a-zA-Z0-9][a-zA-Z0-9.-]{0,251}[a-zA-Z0-9]$ ||
      ! "$desktop" =~ ^d_[a-f0-9]{32}$ ]]; then echo 'ERROR=Invalid domain or Desktop ID' >&2; exit 2; fi
if [ "$(id -u)" -ne 0 ]; then echo 'ERROR=Root or passwordless sudo required' >&2; exit 2; fi
for tool in systemctl python3 tar curl openssl; do
  command -v "$tool" >/dev/null || { echo "ERROR=Missing $tool" >&2; exit 2; }
done
if [ -x /www/server/nginx/sbin/nginx ]; then
  nginx=/www/server/nginx/sbin/nginx
  nginx_conf=/www/server/nginx/conf/nginx.conf
else
  nginx="$(command -v nginx || true)"
  nginx_conf=""
fi
[ -n "$nginx" ] || { echo 'ERROR=Nginx HTTPS site is required' >&2; exit 2; }
nginx_run() {
  if [ -n "$nginx_conf" ]; then "$nginx" "$@" -c "$nginx_conf"; else "$nginx" "$@"; fi
}
site="$(python3 - "$domain" <<'PY'
import glob,os,re,sys
name=sys.argv[1].lower(); matches=[]
for pattern in ('/www/server/panel/vhost/nginx/*.conf',
                '/etc/nginx/sites-enabled/*','/etc/nginx/conf.d/*.conf'):
 for path in glob.glob(pattern):
  if not os.path.isfile(path):continue
  try: data=open(path,encoding='utf-8').read()
  except (OSError,UnicodeError):continue
  if any(name in m.group(1).lower().split()
     for m in re.finditer(r'\bserver_name\s+([^;]+);',data)): matches.append(path)
print(matches[0] if len(matches)==1 else '')
PY
)"
[ -n "$site" ] || { echo 'ERROR=Need exactly one matching Nginx site' >&2; exit 2; }
grep -Eq '\blisten[[:space:]]+443([[:space:];]|$)' "$site" ||
  { echo 'ERROR=Website must already listen on HTTPS 443' >&2; exit 2; }
nginx_run -t >/dev/null 2>&1 ||
  { echo 'ERROR=Existing Nginx config invalid' >&2; exit 2; }
if [ -x /opt/chatx-relay/toolchain/cargo/bin/cargo ]; then
  export CARGO_HOME=/opt/chatx-relay/toolchain/cargo
  export RUSTUP_HOME=/opt/chatx-relay/toolchain/rustup
  export PATH="$CARGO_HOME/bin:$PATH"
fi
cargo_bin="$(command -v cargo || true)"
if [ -z "$cargo_bin" ] || ! "$cargo_bin" --version >/dev/null 2>&1; then
  echo 'ERROR=Rust cargo missing: install rustup stable first' >&2; exit 2
fi
echo "PREFLIGHT_OK site=$site cargo=$("$cargo_bin" --version | head -1)"
if [ "$mode" = check ]; then exit 0; fi
if [ "$mode" != deploy ] || [[ ! "$archive" =~ ^/tmp/chatx-relay-[a-f0-9]{24}\.tar\.gz$ ]] ||
   [ ! -f "$archive" ]; then echo 'ERROR=Missing staged archive' >&2; exit 2; fi
stamp="$(date -u +%Y%m%dT%H%M%SZ)-$$"
backup="/var/backups/chatx-relay/$stamp"
stage="/opt/chatx-relay/releases/$stamp"
install -d -m 700 "$backup"
install -d -m 755 "$stage"
echo "STAGE=backup path=$backup"
service=chatx-relay.service
bin=/usr/local/bin/chatx-relay-server
envfile=/etc/chatx-relay/relay.env
conf=/etc/chatx-relay/chatx-https-mcp.conf
had_service=0; had_env=0; had_bin=0; had_conf=0; had_base=0; site_modified=0; service_created=0
if systemctl cat "$service" >/dev/null 2>&1; then had_service=1; fi
if [ -f "$envfile" ]; then had_env=1; cp -p "$envfile" "$backup/relay.env"; fi
if [ -f "$bin" ]; then had_bin=1; cp -p "$bin" "$backup/server"; fi
cp -p "$site" "$backup/site"
if [ "$had_service" = 1 ]; then
  systemctl cat "$service" >"$backup/service.txt"
  grep -qF "$envfile" "$backup/service.txt" ||
    { echo 'ERROR=Unsupported existing service env path' >&2; exit 2; }
fi
site_name="$(basename "$site" .conf)"
extension="/www/server/panel/vhost/nginx/extension/$site_name"
if [ -d "$extension" ] && grep -qF "$extension/*.conf" "$site"; then
  conf="$extension/chatx-https-mcp.conf"
else
  site_modified=1
fi
base_conf="$(dirname "$conf")/chatx-relay.conf"
if [ "$site_modified" = 1 ] && grep -qF "include $conf;" "$site" &&
   grep -qF "include $base_conf;" "$site"; then
  # A second deploy must not insert duplicate Nginx location includes.
  site_modified=0
fi
if [ -f "$conf" ]; then had_conf=1; cp -p "$conf" "$backup/mcp.conf"; fi
if [ -f "$base_conf" ]; then had_base=1; cp -p "$base_conf" "$backup/base.conf"; fi
rollback() {
  trap - ERR HUP INT TERM
  echo 'STAGE=rollback' >&2
  if [ "$had_bin" = 1 ]; then
    cp -p "$backup/server" "$bin.rollback"
    mv -f "$bin.rollback" "$bin"  # atomic replacement of a running executable
  else rm -f "$bin"; fi
  if [ "$had_env" = 1 ]; then cp -p "$backup/relay.env" "$envfile"; else rm -f "$envfile"; fi
  if [ "$had_conf" = 1 ]; then cp -p "$backup/mcp.conf" "$conf"; else rm -f "$conf"; fi
  if [ "$had_base" = 1 ]; then cp -p "$backup/base.conf" "$base_conf"; else rm -f "$base_conf"; fi
  if [ "$site_modified" = 1 ]; then cp -p "$backup/site" "$site"; fi
  if [ "$service_created" = 1 ]; then
    systemctl stop "$service" || true
    rm -f /etc/systemd/system/chatx-relay.service
    systemctl daemon-reload || true
  elif [ "$had_service" = 1 ]; then systemctl restart "$service" || true; fi
  nginx_run -t && nginx_run -s reload || true
  echo "ERROR=Rolled back; backup=$backup" >&2
  exit 1
}
trap rollback ERR HUP INT TERM
echo 'STAGE=extract'
tar -tzf "$archive" | grep -x 'relay-server/Cargo.toml' >/dev/null
tar -xzf "$archive" -C "$stage"
if [ -f /opt/chatx-relay/build-a7969bb/.cargo/config.toml ]; then
  install -d "$stage/.cargo"
  cp /opt/chatx-relay/build-a7969bb/.cargo/config.toml "$stage/.cargo/config.toml"
fi
echo 'STAGE=compile'
(cd "$stage/relay-server" && "$cargo_bin" build --release --locked)
test -x "$stage/relay-server/target/release/chatx-relay-server"
echo 'STAGE=configure'
install -d -m 700 /etc/chatx-relay
install -d -m 750 /var/lib/chatx-relay
bootstrap=""
if [ "$had_env" = 0 ]; then
  bootstrap="$(openssl rand -hex 32)"
  printf 'CHATX_RELAY_BIND=127.0.0.1:8787\nCHATX_RELAY_DATA=/var/lib/chatx-relay/registry.json\nCHATX_RELAY_BOOTSTRAP_TOKEN=%s\n' "$bootstrap" >"$envfile"
  chmod 600 "$envfile"
fi
if grep -q '^CHATX_MCP_DESKTOP_ID=' "$envfile"; then
  sed -i "s/^CHATX_MCP_DESKTOP_ID=.*/CHATX_MCP_DESKTOP_ID=$desktop/" "$envfile"
else printf 'CHATX_MCP_DESKTOP_ID=%s\n' "$desktop" >>"$envfile"; fi
if [ "$had_service" = 0 ]; then
  id chatx-relay >/dev/null 2>&1 ||
    useradd --system --home /var/lib/chatx-relay --shell /sbin/nologin chatx-relay
  chown chatx-relay:chatx-relay /var/lib/chatx-relay
  cat >/etc/systemd/system/chatx-relay.service <<'EOF'
[Unit]
Description=ChatX Relay
After=network-online.target
Wants=network-online.target
[Service]
Type=simple
User=chatx-relay
Group=chatx-relay
EnvironmentFile=/etc/chatx-relay/relay.env
ExecStart=/usr/local/bin/chatx-relay-server
Restart=always
RestartSec=2
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/lib/chatx-relay
[Install]
WantedBy=multi-user.target
EOF
  service_created=1
  systemctl daemon-reload
  systemctl enable "$service"
fi
install -m 755 "$stage/relay-server/target/release/chatx-relay-server" "$bin.next"
mv -f "$bin.next" "$bin"
install -m 644 "$stage/deploy/nginx/chatx-https-mcp-wss.conf" "$conf.next"
mv -f "$conf.next" "$conf"
if [ "$had_base" = 0 ]; then
  install -m 644 "$stage/deploy/nginx/chatx-relay-base.conf" "$base_conf"
fi
if [ "$site_modified" = 1 ]; then
  python3 - "$site" "$domain" "$conf" "$base_conf" <<'PY'
import re,sys
path,domain,conf,base=sys.argv[1:]
data=open(path,encoding='utf-8').read()
pat=re.compile(r'^(\s*server_name\s+[^;]*\b'+re.escape(domain)+r'\b[^;]*;)',re.M|re.I)
if len(pat.findall(data))!=1:raise SystemExit('Ambiguous server_name')
data=pat.sub(lambda m:m.group(1)+'\n    include '+conf+';\n    include '+base+';',data,count=1)
open(path,'w',encoding='utf-8').write(data)
PY
fi
echo 'STAGE=restart'
nginx_run -t
systemctl restart "$service"
healthy=0
for _ in $(seq 1 20); do
  if curl -fsS --max-time 2 http://127.0.0.1:8787/healthz >/dev/null; then healthy=1; break; fi
  sleep 1
done
test "$healthy" = 1
nginx_run -s reload
echo 'STAGE=verify'
code="$(curl -sS -o /dev/null -w '%{http_code}' --max-time 6 -X POST http://127.0.0.1:8787/mcp)"
if [ "$code" != 401 ] && [ "$code" != 503 ]; then
  echo "ERROR=MCP upstream status=$code" >&2; false
fi
trap - ERR HUP INT TERM
echo "DEPLOY_OK backup=$backup relay=active mcp=$code"
# The launcher handles this secret privately; it must NEVER appear in UI/logs.
if [ -n "$bootstrap" ]; then printf 'CHATX_NEW_BOOTSTRAP_TOKEN=%s\n' "$bootstrap"; fi
