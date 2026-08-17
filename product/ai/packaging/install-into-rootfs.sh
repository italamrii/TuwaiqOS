#!/usr/bin/env bash
# Install Tuwaiq AI userspace package into a rootfs ($1).
# Optional $2: path to a prebuilt tuwaiq-agent-broker binary.
set -euo pipefail

ROOT="${1:-}"
BROKER_BIN="${2:-}"
if [[ -z "${ROOT}" || ! -d "${ROOT}" ]]; then
  echo "usage: install-into-rootfs.sh <rootfs> [broker-binary]" >&2
  exit 2
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
AI_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

install -d "${ROOT}/usr/lib/tuwaiq/ai"
# Agent package (Python) — exclude local caches/tests artifacts if any
rm -rf "${ROOT}/usr/lib/tuwaiq/ai/agent"
install -d "${ROOT}/usr/lib/tuwaiq/ai/agent"
cp -a "${AI_ROOT}/agent/." "${ROOT}/usr/lib/tuwaiq/ai/agent/"
# Drop host-only caches; keep tests for contributor ISO inspection
find "${ROOT}/usr/lib/tuwaiq/ai/agent" -type d -name '__pycache__' -prune -exec rm -rf {} + 2>/dev/null || true
find "${ROOT}/usr/lib/tuwaiq/ai/agent" -type f -name '*.pyc' -delete 2>/dev/null || true

install -d "${ROOT}/usr/libexec/tuwaiq"
if [[ -n "${BROKER_BIN}" && -f "${BROKER_BIN}" ]]; then
  install -m 0755 "${BROKER_BIN}" "${ROOT}/usr/libexec/tuwaiq/tuwaiq-agent-broker"
elif [[ -x "${AI_ROOT}/broker/target/release/tuwaiq-agent-broker" ]]; then
  install -m 0755 "${AI_ROOT}/broker/target/release/tuwaiq-agent-broker" \
    "${ROOT}/usr/libexec/tuwaiq/tuwaiq-agent-broker"
elif [[ -x "${ROOT}/usr/libexec/tuwaiq/tuwaiq-agent-broker" ]]; then
  : # already present
else
  echo "install-into-rootfs: warning: broker binary missing; service will fail until installed" >&2
fi

install -m 0755 "${SCRIPT_DIR}/tuwaiq-ai" "${ROOT}/usr/libexec/tuwaiq/tuwaiq-ai"
install -d "${ROOT}/usr/bin"
ln -sf /usr/libexec/tuwaiq/tuwaiq-ai "${ROOT}/usr/bin/tuwaiq-ai"

install -d "${ROOT}/usr/lib/systemd/system"
install -m 0644 "${SCRIPT_DIR}/tuwaiq-agent-broker.service" \
  "${ROOT}/usr/lib/systemd/system/tuwaiq-agent-broker.service"
install -m 0644 "${SCRIPT_DIR}/tuwaiq-ai.service" \
  "${ROOT}/usr/lib/systemd/system/tuwaiq-ai.service"

install -d "${ROOT}/var/lib/tuwaiq/ai" "${ROOT}/run/tuwaiq" 2>/dev/null || \
  install -d "${ROOT}/var/lib/tuwaiq/ai"

install -d "${ROOT}/usr/share/tuwaiqos"
if [[ -f "${AI_ROOT}/docs/PRODUCT_INTEGRATION.md" ]]; then
  install -m 0644 "${AI_ROOT}/docs/PRODUCT_INTEGRATION.md" \
    "${ROOT}/usr/share/tuwaiqos/AI-PRODUCT.md"
fi

# CRLF hygiene for Windows checkouts
sed -i 's/\r$//' \
  "${ROOT}/usr/lib/systemd/system/tuwaiq-agent-broker.service" \
  "${ROOT}/usr/lib/systemd/system/tuwaiq-ai.service" \
  "${ROOT}/usr/libexec/tuwaiq/tuwaiq-ai" 2>/dev/null || true
chmod 0755 "${ROOT}/usr/libexec/tuwaiq/tuwaiq-ai"
chmod 0644 "${ROOT}/usr/lib/systemd/system/tuwaiq-agent-broker.service" \
           "${ROOT}/usr/lib/systemd/system/tuwaiq-ai.service"

# Enable on multi-user — does not block graphical.target / Plasma.
mkdir -p "${ROOT}/etc/systemd/system/multi-user.target.wants"
ln -sf /usr/lib/systemd/system/tuwaiq-agent-broker.service \
  "${ROOT}/etc/systemd/system/multi-user.target.wants/tuwaiq-agent-broker.service"
ln -sf /usr/lib/systemd/system/tuwaiq-ai.service \
  "${ROOT}/etc/systemd/system/multi-user.target.wants/tuwaiq-ai.service"

echo "Tuwaiq AI userspace installed into ${ROOT}"
