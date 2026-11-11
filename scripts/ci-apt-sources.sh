#!/usr/bin/env bash
# CI only: point apt on a GitHub-hosted ubuntu-24.04 runner at Ubuntu's own archive and security
# servers instead of the image's Azure mirror list (mirror+file:/etc/apt/apt-mirrors.txt). When
# azure.archive.ubuntu.com stops resolving on the runners (2026-11-11), every apt install fails
# and so does every job that installs a package (age, Playwright's system libraries). The packages
# are the same and apt still checks them against Ubuntu's archive key.
set -euo pipefail
. /etc/os-release
src=/etc/apt/sources.list.d/ubuntu.sources
sudo tee "$src" >/dev/null <<SOURCES
Types: deb
URIs: http://archive.ubuntu.com/ubuntu/
Suites: $VERSION_CODENAME $VERSION_CODENAME-updates $VERSION_CODENAME-backports
Components: main restricted universe multiverse
Signed-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg

Types: deb
URIs: http://security.ubuntu.com/ubuntu/
Suites: $VERSION_CODENAME-security
Components: main restricted universe multiverse
Signed-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg
SOURCES
# The image's old-style list, if it has one, would bring the mirror list back.
[ -f /etc/apt/sources.list ] && sudo sed -i '/^deb /s/^/# /' /etc/apt/sources.list
sudo apt-get update -qq
