#!/usr/bin/env bash
set -euo pipefail
case "${1:-}" in
  install)
    test -d /www/server/panel/plugin/chatx_relay
    chmod 700 /www/server/panel/plugin/chatx_relay
    chmod 600 /www/server/panel/plugin/chatx_relay/*.py
    install -m 644 /www/server/panel/plugin/chatx_relay/icon.svg /www/server/panel/BTPanel/static/img/soft_ico/ico-chatx_relay.svg
    echo 'ChatX Relay 管理界面已安装。已有 Relay 服务、密钥及注册记录保留。'
    ;;
  uninstall)
    echo '仅卸载管理界面。Relay 服务、配置及注册电脑保留。'
    ;;
  *) echo 'Usage: install.sh install|uninstall' >&2; exit 2 ;;
esac
