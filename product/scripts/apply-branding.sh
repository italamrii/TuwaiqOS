#!/usr/bin/env bash
# Apply TuwaiqOS D1 branding + connectivity foundation into a target root filesystem ($1).
set -euo pipefail

ROOT="${1:-}"
if [[ -z "${ROOT}" || ! -d "${ROOT}" ]]; then
  echo "usage: apply-branding.sh <rootfs>" >&2
  exit 2
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PRODUCT_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
REPO_ROOT="$(cd "${PRODUCT_ROOT}/.." && pwd)"

if [[ -d "${REPO_ROOT}/product/branding" ]]; then
  BRAND="${REPO_ROOT}/product/branding"
  CFG="${REPO_ROOT}/product/config"
  DESKTOP="${REPO_ROOT}/product/desktop"
  CONN="${REPO_ROOT}/product/connectivity"
else
  BRAND="${PRODUCT_ROOT}/branding"
  CFG="${PRODUCT_ROOT}/config"
  DESKTOP="${PRODUCT_ROOT}/desktop"
  CONN="${PRODUCT_ROOT}/connectivity"
fi

install_tree() {
  local src="$1" dst="$2"
  install -d "$(dirname "${dst}")"
  cp -a "${src}" "${dst}"
}

# --- Identity ---
install -d "${ROOT}/usr/share/plasma/look-and-feel"
cp -a "${BRAND}/plasma/look-and-feel/org.tuwaiqos.desktop" \
  "${ROOT}/usr/share/plasma/look-and-feel/"
cp -a "${BRAND}/plasma/look-and-feel/org.tuwaiqos.light.desktop" \
  "${ROOT}/usr/share/plasma/look-and-feel/"

install -d "${ROOT}/usr/share/color-schemes"
cp -a "${BRAND}/color-schemes/TuwaiqDark.colors" "${ROOT}/usr/share/color-schemes/"
cp -a "${BRAND}/color-schemes/TuwaiqLight.colors" "${ROOT}/usr/share/color-schemes/"

install -d "${ROOT}/usr/share/wallpapers"
cp -a "${BRAND}/wallpapers/TuwaiqOS" "${ROOT}/usr/share/wallpapers/"

install -d "${ROOT}/usr/share/tuwaiqos/icons"
cp -a "${BRAND}/icons/tuwaiq-mark.svg" "${ROOT}/usr/share/tuwaiqos/icons/"
cp -a "${BRAND}/icons/tuwaiq-wordmark.svg" "${ROOT}/usr/share/tuwaiqos/icons/"

# Plasma defaults (system + skel + existing user)
install -d "${ROOT}/etc/xdg"
cp "${DESKTOP}/plasma/kdeglobals" "${ROOT}/etc/xdg/kdeglobals"
cp "${DESKTOP}/plasma/kwinrc" "${ROOT}/etc/xdg/kwinrc"
cp "${DESKTOP}/plasma/kscreenlockerrc" "${ROOT}/etc/xdg/kscreenlockerrc"
cp "${DESKTOP}/plasma/plasma-org.kde.plasma.desktop-appletsrc" \
  "${ROOT}/etc/xdg/plasma-org.kde.plasma.desktop-appletsrc"

install -d "${ROOT}/etc/skel/.config"
cp "${DESKTOP}/plasma/kdeglobals" "${ROOT}/etc/skel/.config/kdeglobals"
cp "${DESKTOP}/plasma/kwinrc" "${ROOT}/etc/skel/.config/kwinrc"
cp "${DESKTOP}/plasma/kscreenlockerrc" "${ROOT}/etc/skel/.config/kscreenlockerrc"
cp "${DESKTOP}/plasma/plasma-org.kde.plasma.desktop-appletsrc" \
  "${ROOT}/etc/skel/.config/plasma-org.kde.plasma.desktop-appletsrc"

# Konsole
install -d "${ROOT}/usr/share/konsole"
cp "${DESKTOP}/konsole/Tuwaiq.profile" "${ROOT}/usr/share/konsole/"
cp "${DESKTOP}/konsole/Tuwaiq.colorscheme" "${ROOT}/usr/share/konsole/"
cp "${DESKTOP}/konsole/konsolerc" "${ROOT}/etc/xdg/konsolerc"
install -d "${ROOT}/etc/skel/.local/share/konsole"
cp "${DESKTOP}/konsole/Tuwaiq.profile" "${ROOT}/etc/skel/.local/share/konsole/"
cp "${DESKTOP}/konsole/Tuwaiq.colorscheme" "${ROOT}/etc/skel/.local/share/konsole/"
cp "${DESKTOP}/konsole/konsolerc" "${ROOT}/etc/skel/.config/konsolerc"

# Apply identity into existing tuwaiq user home when present
if [[ -d "${ROOT}/home/tuwaiq" ]]; then
  install -d "${ROOT}/home/tuwaiq/.config"
  install -d "${ROOT}/home/tuwaiq/.local/share/konsole"
  cp "${DESKTOP}/plasma/kdeglobals" "${ROOT}/home/tuwaiq/.config/kdeglobals"
  cp "${DESKTOP}/plasma/kwinrc" "${ROOT}/home/tuwaiq/.config/kwinrc"
  cp "${DESKTOP}/plasma/kscreenlockerrc" "${ROOT}/home/tuwaiq/.config/kscreenlockerrc"
  cp "${DESKTOP}/plasma/plasma-org.kde.plasma.desktop-appletsrc" \
    "${ROOT}/home/tuwaiq/.config/plasma-org.kde.plasma.desktop-appletsrc"
  cp "${DESKTOP}/konsole/konsolerc" "${ROOT}/home/tuwaiq/.config/konsolerc"
  cp "${DESKTOP}/konsole/Tuwaiq.profile" "${ROOT}/home/tuwaiq/.local/share/konsole/"
  cp "${DESKTOP}/konsole/Tuwaiq.colorscheme" "${ROOT}/home/tuwaiq/.local/share/konsole/"
  chown -R 1000:1000 "${ROOT}/home/tuwaiq/.config" "${ROOT}/home/tuwaiq/.local" 2>/dev/null || true
fi

# SDDM (Breeze greeter + Tuwaiq wallpaper — preserves D0 login reliability)
install -d "${ROOT}/etc/sddm.conf.d"
cp "${DESKTOP}/sddm/tuwaiqos.conf" "${ROOT}/etc/sddm.conf.d/tuwaiqos.conf"
install -d "${ROOT}/usr/share/sddm/themes/breeze"
cp "${DESKTOP}/sddm/breeze-theme.conf.user" \
  "${ROOT}/usr/share/sddm/themes/breeze/theme.conf.user"
# Optional custom theme kept for future polish (not selected by default)
if [[ -d "${DESKTOP}/sddm/theme" ]]; then
  install -d "${ROOT}/usr/share/sddm/themes/tuwaiqos"
  cp -a "${DESKTOP}/sddm/theme/." "${ROOT}/usr/share/sddm/themes/tuwaiqos/"
fi

# Hostname + OS presentation
echo "tuwaiqos" > "${ROOT}/etc/hostname"
if [[ -f "${ROOT}/etc/hosts" ]]; then
  if ! grep -q 'tuwaiqos' "${ROOT}/etc/hosts"; then
    printf '\n127.0.1.1\ttuwaiqos\n' >> "${ROOT}/etc/hosts"
  fi
fi

if [[ -f "${ROOT}/etc/os-release" ]]; then
  cp "${ROOT}/etc/os-release" "${ROOT}/etc/os-release.ubuntu-base"
fi
cat > "${ROOT}/etc/os-release" <<'EOF'
PRETTY_NAME="TuwaiqOS"
NAME="TuwaiqOS"
ID=tuwaiqos
ID_LIKE="ubuntu debian"
VERSION_ID="d1"
VERSION="D1 (Desktop Identity + Connectivity)"
HOME_URL="https://github.com/italamrii/TuwaiqOS"
SUPPORT_URL="https://github.com/italamrii/TuwaiqOS"
BUG_REPORT_URL="https://github.com/italamrii/TuwaiqOS/issues"
PRIVACY_POLICY_URL="https://github.com/italamrii/TuwaiqOS"
UBUNTU_CODENAME=noble
EOF

# LSB / machine-info presentation for About screens
cat > "${ROOT}/etc/lsb-release" <<'EOF'
DISTRIB_ID=TuwaiqOS
DISTRIB_RELEASE=d1
DISTRIB_CODENAME=d1
DISTRIB_DESCRIPTION="TuwaiqOS D1"
EOF

cat > "${ROOT}/etc/machine-info" <<'EOF'
PRETTY_HOSTNAME=TuwaiqOS
ICON_NAME=computer
CHASSIS=desktop
DEPLOYMENT=development
EOF

install -d "${ROOT}/etc/xdg/tuwaiqos"
cp "${CFG}/tuwaiqos.conf" "${ROOT}/etc/xdg/tuwaiqos/tuwaiqos.conf"
cp "${CFG}/locale.conf" "${ROOT}/etc/locale.conf"

# Keep systemd-resolved stub symlink healthy (chroot/apt can replace this with a flat file)
if [[ -d "${ROOT}/run/systemd/resolve" ]] || [[ -L "${ROOT}/etc/resolv.conf" ]] || [[ -e "${ROOT}/etc/resolv.conf" ]]; then
  rm -f "${ROOT}/etc/resolv.conf"
  ln -s ../run/systemd/resolve/stub-resolv.conf "${ROOT}/etc/resolv.conf"
fi

cat > "${ROOT}/usr/share/tuwaiqos/README-branding.txt" <<'EOF'
TuwaiqOS D1 branding applied.
Look-and-feel: org.tuwaiqos.desktop (Dark) / org.tuwaiqos.light.desktop (Light)
Wallpaper: TuwaiqOS cinematic escarpment
Session: Plasma X11 via SDDM (D0 chain preserved)
Color schemes: TuwaiqDark / TuwaiqLight
EOF

# --- Connectivity foundation ---
install -d "${ROOT}/usr/libexec/tuwaiq"
install -m 0755 "${CONN}/tuwaiq-connectivity-status.sh" \
  "${ROOT}/usr/libexec/tuwaiq/tuwaiq-connectivity-status.sh"
install -m 0755 "${CONN}/measure-network.sh" \
  "${ROOT}/usr/libexec/tuwaiq/measure-network.sh"
install -m 0755 "${CONN}/d1-acceptance-collect.sh" \
  "${ROOT}/usr/libexec/tuwaiq/d1-acceptance-collect.sh"
install -m 0755 "${CONN}/d1-dns-closure.sh" \
  "${ROOT}/usr/libexec/tuwaiq/d1-dns-closure.sh"
install -m 0755 "${CONN}/d1-theme-visual.sh" \
  "${ROOT}/usr/libexec/tuwaiq/d1-theme-visual.sh"
install -d "${ROOT}/usr/lib/systemd/system"
cp "${CONN}/tuwaiq-connectivity-status.service" "${ROOT}/usr/lib/systemd/system/"
cp "${CONN}/tuwaiq-connectivity-status.timer" "${ROOT}/usr/lib/systemd/system/"
cp "${CONN}/tuwaiq-d1-acceptance.service" "${ROOT}/usr/lib/systemd/system/"
cp "${CONN}/tuwaiq-d1-dns-closure.service" "${ROOT}/usr/lib/systemd/system/"
cp "${CONN}/tuwaiq-d1-theme-visual.service" "${ROOT}/usr/lib/systemd/system/"
chmod 0644 "${ROOT}/usr/lib/systemd/system/tuwaiq-"*.service \
           "${ROOT}/usr/lib/systemd/system/tuwaiq-"*.timer 2>/dev/null || true
install -d "${ROOT}/etc/NetworkManager"
cp "${CONN}/NetworkManager.conf" "${ROOT}/etc/NetworkManager/NetworkManager.conf"
install -d "${ROOT}/etc/NetworkManager/system-connections"
install -m 0600 "${CONN}/tuwaiq-wired.nmconnection" \
  "${ROOT}/etc/NetworkManager/system-connections/tuwaiq-wired.nmconnection"
# Netplan → NetworkManager (Ubuntu 24.04 expects a renderer; empty netplan leaves NICs down)
install -d "${ROOT}/etc/netplan"
install -m 0644 "${CONN}/01-tuwaiq-network.yaml" \
  "${ROOT}/etc/netplan/01-tuwaiq-network.yaml"
install -d "${ROOT}/etc/systemd/resolved.conf.d"
install -m 0644 "${CONN}/tuwaiq-resolved.conf" \
  "${ROOT}/etc/systemd/resolved.conf.d/tuwaiq-dns.conf"
# Ensure systemd units are not marked executable (avoids systemd warnings)
chmod 0644 "${ROOT}/usr/lib/systemd/system/tuwaiq-connectivity-status.service" \
           "${ROOT}/usr/lib/systemd/system/tuwaiq-connectivity-status.timer" \
           "${ROOT}/usr/lib/systemd/system/tuwaiq-d1-acceptance.service" \
           "${ROOT}/usr/lib/systemd/system/tuwaiq-d1-dns-closure.service" \
           "${ROOT}/usr/lib/systemd/system/tuwaiq-d1-theme-visual.service" 2>/dev/null || true
# Windows checkouts may copy CRLF; systemd unit parser requires LF
sed -i 's/\r$//' "${ROOT}/usr/lib/systemd/system/tuwaiq-"*.service \
                 "${ROOT}/usr/lib/systemd/system/tuwaiq-"*.timer \
                 "${ROOT}/usr/libexec/tuwaiq/"*.sh 2>/dev/null || true
# Normal getaddrinfo via nss-resolve when the package is present
if [[ -f "${ROOT}/usr/lib/x86_64-linux-gnu/libnss_resolve.so.2" ]] || \
   [[ -f "${ROOT}/lib/x86_64-linux-gnu/libnss_resolve.so.2" ]]; then
  sed -i 's/^hosts:.*/hosts: files resolve [!UNAVAIL=return] dns/' \
    "${ROOT}/etc/nsswitch.conf"
fi
install -d "${ROOT}/etc/NetworkManager/dispatcher.d"
install -m 0755 "${CONN}/99-tuwaiq-connectivity" \
  "${ROOT}/etc/NetworkManager/dispatcher.d/99-tuwaiq-connectivity"
install -d "${ROOT}/var/lib/tuwaiq/connectivity"
chmod 0755 "${ROOT}/var/lib/tuwaiq" "${ROOT}/var/lib/tuwaiq/connectivity"
# Clear one-shot markers so clean-boot proofs re-run
rm -f "${ROOT}/var/lib/tuwaiq/connectivity/d1-acceptance.done" \
      "${ROOT}/var/lib/tuwaiq/connectivity/d1-dns-closure.done" \
      "${ROOT}/var/lib/tuwaiq/connectivity/d1-theme-visual.done"

# Enable connectivity timer + NetworkManager + resolved + closure collectors
mkdir -p "${ROOT}/etc/systemd/system/timers.target.wants" \
         "${ROOT}/etc/systemd/system/multi-user.target.wants" \
         "${ROOT}/etc/systemd/system/network-online.target.wants"
ln -sf /usr/lib/systemd/system/tuwaiq-connectivity-status.timer \
  "${ROOT}/etc/systemd/system/timers.target.wants/tuwaiq-connectivity-status.timer"
ln -sf /usr/lib/systemd/system/tuwaiq-d1-acceptance.service \
  "${ROOT}/etc/systemd/system/multi-user.target.wants/tuwaiq-d1-acceptance.service"
ln -sf /usr/lib/systemd/system/tuwaiq-d1-dns-closure.service \
  "${ROOT}/etc/systemd/system/multi-user.target.wants/tuwaiq-d1-dns-closure.service"
ln -sf /usr/lib/systemd/system/tuwaiq-d1-theme-visual.service \
  "${ROOT}/etc/systemd/system/multi-user.target.wants/tuwaiq-d1-theme-visual.service"
ln -sf /lib/systemd/system/NetworkManager.service \
  "${ROOT}/etc/systemd/system/multi-user.target.wants/NetworkManager.service" 2>/dev/null || true
# systemd-resolved owns the stub resolver (required for normal getaddrinfo via /etc/resolv.conf)
if [[ -f "${ROOT}/lib/systemd/system/systemd-resolved.service" ]]; then
  RESOLVED_UNIT=/lib/systemd/system/systemd-resolved.service
elif [[ -f "${ROOT}/usr/lib/systemd/system/systemd-resolved.service" ]]; then
  RESOLVED_UNIT=/usr/lib/systemd/system/systemd-resolved.service
else
  RESOLVED_UNIT=""
fi
if [[ -n "${RESOLVED_UNIT}" ]]; then
  ln -sf "${RESOLVED_UNIT}" \
    "${ROOT}/etc/systemd/system/multi-user.target.wants/systemd-resolved.service"
  ln -sf "${RESOLVED_UNIT}" \
    "${ROOT}/etc/systemd/system/dbus-org.freedesktop.resolve1.service"
fi
ln -sf /lib/systemd/system/NetworkManager-wait-online.service \
  "${ROOT}/etc/systemd/system/network-online.target.wants/NetworkManager-wait-online.service" 2>/dev/null || \
  ln -sf /usr/lib/systemd/system/NetworkManager-wait-online.service \
    "${ROOT}/etc/systemd/system/network-online.target.wants/NetworkManager-wait-online.service" 2>/dev/null || true

# Default-deny firewall (ufw) when package present — do not fail branding if absent
if [[ -x "${ROOT}/usr/sbin/ufw" ]] || [[ -e "${ROOT}/usr/sbin/ufw" ]]; then
  mkdir -p "${ROOT}/etc/ufw"
  cat > "${ROOT}/etc/ufw/ufw.conf" <<'EOF'
# /etc/ufw/ufw.conf
ENABLED=yes
LOGLEVEL=low
EOF
  # Prefers ipv6; default deny incoming / allow outgoing is ufw package default.
  chroot "${ROOT}" ufw --force reset >/dev/null 2>&1 || true
  chroot "${ROOT}" ufw default deny incoming >/dev/null 2>&1 || true
  chroot "${ROOT}" ufw default allow outgoing >/dev/null 2>&1 || true
  chroot "${ROOT}" ufw --force enable >/dev/null 2>&1 || true
  mkdir -p "${ROOT}/etc/systemd/system/multi-user.target.wants"
  ln -sf /lib/systemd/system/ufw.service \
    "${ROOT}/etc/systemd/system/multi-user.target.wants/ufw.service" 2>/dev/null || true
fi

# Ensure plasma-nm indicator package note
cat > "${ROOT}/usr/share/tuwaiqos/CONNECTIVITY.txt" <<'EOF'
TuwaiqOS D1 connectivity foundation
Authority: NetworkManager
Desktop indicator: plasma-nm
Status JSON: /var/lib/tuwaiq/connectivity/status.json (also /run/tuwaiq/connectivity.json)
Future AI boundary: read status JSON only — never unrestricted root.
Wi-Fi: infrastructure packages may be present; hardware support is NOT claimed without real-device tests.
EOF

# --- Tuwaiq AI userspace (non-boot-critical) ---
AI_PKG="${PRODUCT_ROOT}/ai/packaging"
if [[ -x "${AI_PKG}/install-into-rootfs.sh" ]] || [[ -f "${AI_PKG}/install-into-rootfs.sh" ]]; then
  sed -i 's/\r$//' "${AI_PKG}/install-into-rootfs.sh" 2>/dev/null || true
  bash "${AI_PKG}/install-into-rootfs.sh" "${ROOT}" \
    "${TUWAIQ_AI_BROKER_BIN:-}"
else
  echo "apply-branding: AI packaging script missing; skipping AI install" >&2
fi

echo "Tuwaiq D1 branding + connectivity applied to ${ROOT}"
