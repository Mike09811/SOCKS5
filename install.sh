#!/usr/bin/env bash
# sk5 一键安装引导脚本:下载对应架构的 sk5-install 并执行,参数原样透传。
# 用法:
#   curl -fsSL https://raw.githubusercontent.com/Mike09811/sk5/main/install.sh | sudo bash
#   curl -fsSL … | sudo bash -s -- --bbr native --port 1080
#
# 仓库可用环境变量 SK5_REPO 覆盖。
set -euo pipefail

REPO="${SK5_REPO:-Mike09811/sk5}"

main() {
    if [ "$(id -u)" != 0 ]; then
        echo "✘ 请以 root 运行(如: sudo bash install.sh)" >&2
        exit 1
    fi
    case "$(uname -m)" in
        x86_64)  arch="x86_64" ;;
        aarch64 | arm64) arch="aarch64" ;;
        *) echo "✘ 不支持的架构: $(uname -m)" >&2; exit 1 ;;
    esac

    url="https://github.com/${REPO}/releases/latest/download/sk5-install-${arch}"
    tmp="$(mktemp /tmp/sk5-install.XXXXXX)"
    trap 'rm -f "$tmp"' EXIT

    if command -v curl >/dev/null 2>&1; then
        curl -fsSL "$url" -o "$tmp"
    elif command -v wget >/dev/null 2>&1; then
        wget -qO "$tmp" "$url"
    elif command -v apt-get >/dev/null 2>&1; then
        apt-get update -y && apt-get install -y curl && curl -fsSL "$url" -o "$tmp"
    elif command -v yum >/dev/null 2>&1; then
        yum install -y curl && curl -fsSL "$url" -o "$tmp"
    else
        echo "✘ 系统缺少 curl/wget 且无法自动安装" >&2
        exit 1
    fi

    chmod +x "$tmp"
    exec "$tmp" "$@"
}

main "$@"
