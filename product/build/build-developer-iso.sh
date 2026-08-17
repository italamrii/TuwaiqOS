#!/usr/bin/env bash
# Build the TuwaiqOS Developer ISO (live) from the existing Plasma rootfs.
#
# This does NOT debootstrap a new system. It copies the qualified D0/D1 rootfs,
# adds live-boot plus contributor tooling (git, ssh, editors), and packs a
# hybrid BIOS+UEFI ISO with grub-mkrescue.
#
# Run privileged in Docker with the work volume mounted at /work, the repo at
# /src, and the output directory at /out.
set -euo pipefail

SRC_ROOTFS="${SRC_ROOTFS:-/work/rootfs}"      # baseline, never modified
ISO_ROOT="${ISO_ROOT:-/work/iso-rootfs}"      # our working copy
ISO_TREE="${ISO_TREE:-/work/iso-tree}"
OUT_ISO="${OUT_ISO:-/out/TuwaiqOS-Developer-x86_64.iso}"
STAGE="${ISO_ROOT}/.iso-stage"
MIRROR="${TUWAIQ_UBUNTU_MIRROR:-http://azure.archive.ubuntu.com/ubuntu}"

export DEBIAN_FRONTEND=noninteractive

log() { printf '[iso] %s\n' "$*"; }
die() { printf '[iso] ERROR: %s\n' "$*" >&2; exit 1; }

[[ -d "${SRC_ROOTFS}" ]] || die "missing source rootfs ${SRC_ROOTFS}"

log "install builder tooling"
apt-get update -qq
apt-get install -y -qq --no-install-recommends \
  rsync squashfs-tools xorriso grub-common grub-pc-bin grub-efi-amd64-bin \
  mtools dosfstools ca-certificates >/dev/null

chroot_mount() {
  mount --bind /dev "${ISO_ROOT}/dev" 2>/dev/null || true
  mount --bind /proc "${ISO_ROOT}/proc" 2>/dev/null || true
  mount --bind /sys "${ISO_ROOT}/sys" 2>/dev/null || true
  mkdir -p "${ISO_ROOT}/dev/pts"
  mount -t devpts devpts "${ISO_ROOT}/dev/pts" 2>/dev/null || true
}
chroot_umount() {
  umount "${ISO_ROOT}/dev/pts" 2>/dev/null || true
  umount "${ISO_ROOT}/dev" 2>/dev/null || true
  umount "${ISO_ROOT}/proc" 2>/dev/null || true
  umount "${ISO_ROOT}/sys" 2>/dev/null || true
}

# ---------------------------------------------------------------- copy rootfs
if [[ -f "${STAGE}" ]] && grep -qx 'rootfs-copied' "${STAGE}"; then
  log "resume: reusing ${ISO_ROOT}"
else
  log "copy ${SRC_ROOTFS} -> ${ISO_ROOT} (baseline stays untouched)"
  # --delete keeps a partial copy from an interrupted run consistent with the
  # source instead of forcing a full re-copy.
  mkdir -p "${ISO_ROOT}"
  rsync -aHAX --numeric-ids --delete "${SRC_ROOTFS}/" "${ISO_ROOT}/"
  echo 'rootfs-copied' > "${STAGE}"
fi

# ------------------------------------------------------------ add live + dev
if grep -qx 'packages-added' "${STAGE}"; then
  log "resume: live-boot and contributor packages already installed"
else
  cat > "${ISO_ROOT}/etc/apt/sources.list" <<EOF
deb ${MIRROR} noble main restricted universe multiverse
deb ${MIRROR} noble-updates main restricted universe multiverse
deb http://security.ubuntu.com/ubuntu noble-security main restricted universe multiverse
EOF

  chroot_mount
  trap chroot_umount EXIT

  log "apt update inside ISO rootfs"
  attempt=1
  until chroot "${ISO_ROOT}" apt-get -o Acquire::Retries=5 -o Acquire::http::Timeout=60 update; do
    (( attempt >= 3 )) && die "apt update failed inside ISO rootfs"
    log "apt update attempt ${attempt} failed; retrying"
    attempt=$((attempt + 1)); sleep $((attempt * 10))
  done

  # live-boot gives boot=live overlay support for the already-configured rootfs.
  # live-config is deliberately NOT installed: this rootfs is already configured
  # (tuwaiq user, SDDM autologin, Tuwaiq branding) and live-config would fight it.
  # Only add live-image/developer dependencies absent from the existing D1
  # rootfs. Keep the installed Plasma package set intact.
  PKGS=(live-boot live-boot-initramfs-tools git openssh-client nano vim less htop unzip wget ca-certificates systemd-resolved libnss-resolve python3)
  log "install: ${PKGS[*]}"
  attempt=1
  until chroot "${ISO_ROOT}" apt-get -o Acquire::Retries=5 install -y --no-install-recommends "${PKGS[@]}"; do
    (( attempt >= 3 )) && die "apt install failed inside ISO rootfs"
    log "apt install attempt ${attempt} failed; retrying"
    chroot "${ISO_ROOT}" apt-get -f install -y || true
    attempt=$((attempt + 1)); sleep $((attempt * 10))
  done

  chroot "${ISO_ROOT}" apt-get clean
  chroot_umount
  trap - EXIT
  echo 'packages-added' >> "${STAGE}"
fi

# ------------------------------------------------------------- build AI broker
# Native Linux broker binary for Product /proc evidence (not Docker telemetry).
# Copy out of the repo tree so Cargo does not inherit the kernel workspace
# .cargo/config.toml (build-std / custom target).
BROKER_SRC="/src/product/ai/broker"
BROKER_BUILD="/tmp/tuwaiq-agent-broker-src"
BROKER_OUT="/work/tuwaiq-agent-broker"
if [[ -x "${BROKER_OUT}" ]] && grep -qx 'ai-broker-built' "${STAGE}" 2>/dev/null; then
  log "resume: reusing ${BROKER_OUT}"
else
  log "build tuwaiq-agent-broker (release) for Product rootfs"
  apt-get install -y -qq --no-install-recommends cargo rustc build-essential pkg-config >/dev/null
  rm -rf "${BROKER_BUILD}"
  mkdir -p "${BROKER_BUILD}"
  rsync -a --delete \
    --exclude target \
    --exclude .cargo \
    "${BROKER_SRC}/" "${BROKER_BUILD}/"
  ( cd "${BROKER_BUILD}" && CARGO_HOME="${BROKER_BUILD}/.cargo-home" cargo build --release )
  install -m 0755 "${BROKER_BUILD}/target/release/tuwaiq-agent-broker" "${BROKER_OUT}"
  echo 'ai-broker-built' >> "${STAGE}"
fi
export TUWAIQ_AI_BROKER_BIN="${BROKER_OUT}"

# Ensure python3 present even on resumed ISO rootfs builds
if [[ ! -x "${ISO_ROOT}/usr/bin/python3" ]]; then
  log "install python3 for tuwaiq-ai service"
  chroot_mount
  trap chroot_umount EXIT
  chroot "${ISO_ROOT}" apt-get -o Acquire::Retries=3 install -y --no-install-recommends python3 || true
  chroot_umount
  trap - EXIT
fi

# ------------------------------------------------------------- live tailoring
log "tailor rootfs for live boot"

# Apply the same D1 identity and NetworkManager/netplan configuration used by
# the qualified product disk. This operates only on ISO_ROOT, never SRC_ROOTFS.
log "apply existing D1 branding and NetworkManager configuration"
apply_branding_backup="$(mktemp)"
cp /src/product/scripts/apply-branding.sh "${apply_branding_backup}"
sed -i 's/\r$//' /src/product/scripts/apply-branding.sh
if ! bash /src/product/scripts/apply-branding.sh "${ISO_ROOT}"; then
  cp "${apply_branding_backup}" /src/product/scripts/apply-branding.sh
  rm -f "${apply_branding_backup}"
  die "apply-branding failed"
fi
cp "${apply_branding_backup}" /src/product/scripts/apply-branding.sh
rm -f "${apply_branding_backup}"

# Ensure AI units enabled even if branding was partially resumed from older tree
if [[ -f "${ISO_ROOT}/usr/lib/systemd/system/tuwaiq-ai.service" ]]; then
  mkdir -p "${ISO_ROOT}/etc/systemd/system/multi-user.target.wants"
  ln -sf /usr/lib/systemd/system/tuwaiq-agent-broker.service \
    "${ISO_ROOT}/etc/systemd/system/multi-user.target.wants/tuwaiq-agent-broker.service"
  ln -sf /usr/lib/systemd/system/tuwaiq-ai.service \
    "${ISO_ROOT}/etc/systemd/system/multi-user.target.wants/tuwaiq-ai.service"
  # Re-install broker binary if branding ran without TUWAIQ_AI_BROKER_BIN
  if [[ -x "${BROKER_OUT}" ]]; then
    install -m 0755 "${BROKER_OUT}" "${ISO_ROOT}/usr/libexec/tuwaiq/tuwaiq-agent-broker"
  fi
fi
# `apply-branding.sh` installs the resolver stub symlink when it exists; make
# the live-image contract explicit after the resolver packages are installed.
rm -f "${ISO_ROOT}/etc/resolv.conf"
ln -s ../run/systemd/resolve/stub-resolv.conf "${ISO_ROOT}/etc/resolv.conf"
chroot "${ISO_ROOT}" systemctl enable NetworkManager.service systemd-resolved.service >/dev/null 2>&1 || true

# Live root comes from the squashfs overlay; disk PARTUUID entries would fail.
cat > "${ISO_ROOT}/etc/fstab" <<'EOF'
# TuwaiqOS Developer ISO (live) — root is provided by live-boot overlay.
EOF

# D1 proof harnesses are acceptance tooling, not contributor features.
for unit in tuwaiq-d1-theme-visual tuwaiq-d1-dns-closure tuwaiq-d1-acceptance; do
  rm -f "${ISO_ROOT}/etc/systemd/system/multi-user.target.wants/${unit}.service"
done

: > "${ISO_ROOT}/etc/machine-id"
rm -f "${ISO_ROOT}/var/lib/dbus/machine-id"

echo "tuwaiqos-dev" > "${ISO_ROOT}/etc/hostname"
sed -i 's/^127\.0\.1\.1.*/127.0.1.1\ttuwaiqos-dev/' "${ISO_ROOT}/etc/hosts" 2>/dev/null || true

# Graphical target + autologin so the ISO lands on the desktop unattended.
chroot "${ISO_ROOT}" systemctl set-default graphical.target >/dev/null 2>&1 || true
mkdir -p "${ISO_ROOT}/etc/sddm.conf.d"
# Match the D0/D1-qualified identifier. The X11 session file is
# /usr/share/xsessions/plasma.desktop; SDDM's Session= value is "plasma".
cat > "${ISO_ROOT}/etc/sddm.conf.d/autologin.conf" <<'EOF'
[Autologin]
User=tuwaiq
Session=plasma
EOF

# Ensure the user's shipped panel/wallpaper configuration wins over stale
# runtime cache from the source rootfs.
rm -rf "${ISO_ROOT}/home/tuwaiq/.cache/plasmashell" \
       "${ISO_ROOT}/home/tuwaiq/.cache/plasma"* \
       "${ISO_ROOT}/home/tuwaiq/.local/share/plasma"
chroot "${ISO_ROOT}" chown -R tuwaiq:tuwaiq /home/tuwaiq >/dev/null 2>&1 || true

# ------------------------------------------------- contributor docs on the ISO
log "install contributor documentation"
install -d "${ISO_ROOT}/usr/share/tuwaiqos"
install -m 0644 /src/product/iso/README-contributors.md \
  "${ISO_ROOT}/usr/share/tuwaiqos/README-contributors.md"

install -d "${ISO_ROOT}/usr/share/tuwaiqos/gui-source"
for d in product/gui product/branding product/desktop; do
  rsync -a "/src/${d}" "${ISO_ROOT}/usr/share/tuwaiqos/gui-source/" 2>/dev/null || true
done
install -m 0644 /src/product/scripts/apply-branding.sh \
  "${ISO_ROOT}/usr/share/tuwaiqos/gui-source/apply-branding.sh" 2>/dev/null || true

for home in "${ISO_ROOT}/home/tuwaiq" "${ISO_ROOT}/etc/skel"; do
  install -d "${home}/Desktop"
  install -m 0644 /src/product/iso/README-contributors.md \
    "${home}/Desktop/README-contributors.md"
done
chroot "${ISO_ROOT}" chown -R tuwaiq:tuwaiq /home/tuwaiq >/dev/null 2>&1 || true

# ------------------------------------------------------------------- initramfs
log "regenerate initramfs with live-boot hooks"
chroot_mount
trap chroot_umount EXIT
chroot "${ISO_ROOT}" update-initramfs -u -k all
chroot_umount
trap - EXIT

KVER="$(basename "$(ls -1 "${ISO_ROOT}"/boot/vmlinuz-* | tail -1)" | sed 's/^vmlinuz-//')"
log "kernel=${KVER}"
lsinitramfs_out="$(chroot "${ISO_ROOT}" lsinitramfs "/boot/initrd.img-${KVER}" 2>/dev/null | grep -c 'live' || true)"
log "live hooks present in initrd: ${lsinitramfs_out}"
[[ "${lsinitramfs_out}" -gt 0 ]] || die "initrd has no live-boot hooks; live ISO would not boot"

# ------------------------------------------------------------------ ISO tree
log "assemble ISO tree"
rm -rf "${ISO_TREE}"
mkdir -p "${ISO_TREE}/live" "${ISO_TREE}/boot/grub"
cp "${ISO_ROOT}/boot/vmlinuz-${KVER}" "${ISO_TREE}/live/vmlinuz"
cp "${ISO_ROOT}/boot/initrd.img-${KVER}" "${ISO_TREE}/live/initrd.img"

cat > "${ISO_TREE}/boot/grub/grub.cfg" <<'EOF'
set timeout=10
set default=0

insmod all_video
insmod gfxterm
serial --unit=0 --speed=115200
terminal_input console serial
terminal_output console serial

menuentry "TuwaiqOS Developer (Live)" {
  linux /live/vmlinuz boot=live components quiet splash console=tty0 console=ttyS0,115200n8
  initrd /live/initrd.img
}

menuentry "TuwaiqOS Developer (Live, verbose)" {
  linux /live/vmlinuz boot=live components console=tty0 console=ttyS0,115200n8
  initrd /live/initrd.img
}

menuentry "TuwaiqOS Developer (Live, safe graphics)" {
  linux /live/vmlinuz boot=live components nomodeset console=tty0 console=ttyS0,115200n8
  initrd /live/initrd.img
}
EOF

log "build squashfs (this is the slow step)"
# Cap threads and memory: unbounded xz compression has OOM-killed the Docker
# Desktop VM on this host mid-build.
if [[ -s "${SQUASH_CACHE:-/work/filesystem.squashfs}" ]] && [[ "${REUSE_SQUASHFS:-1}" == "1" ]]; then
  log "resume: reusing ${SQUASH_CACHE:-/work/filesystem.squashfs}"
  cp "${SQUASH_CACHE:-/work/filesystem.squashfs}" "${ISO_TREE}/live/filesystem.squashfs"
else
  rm -f "${SQUASH_CACHE:-/work/filesystem.squashfs}"
  mksquashfs "${ISO_ROOT}" "${SQUASH_CACHE:-/work/filesystem.squashfs}" \
    -comp xz -noappend -no-progress -wildcards \
    -processors "${SQUASH_PROCS:-2}" -mem "${SQUASH_MEM:-768M}" \
    -e '.iso-stage' 'proc/*' 'sys/*' 'dev/pts/*' 'tmp/*' 'var/cache/apt/archives/*.deb'
  cp "${SQUASH_CACHE:-/work/filesystem.squashfs}" "${ISO_TREE}/live/filesystem.squashfs"
fi

printf 'TuwaiqOS Developer (live)\n' > "${ISO_TREE}/.disk-info" || true

# --------------------------------------------------------------------- xorriso
log "build hybrid BIOS+UEFI ISO with grub-mkrescue"
mkdir -p "$(dirname "${OUT_ISO}")"
rm -f "${OUT_ISO}"
# Options after `--` are passed through to xorriso; grub-mkrescue itself does
# not accept --volid.
grub-mkrescue --output="${OUT_ISO}" "${ISO_TREE}" -- -volid TUWAIQOS_DEV

[[ -s "${OUT_ISO}" ]] || die "ISO not produced"

log "ISO structure"
xorriso -indev "${OUT_ISO}" -toc 2>/dev/null | tail -20 || true
echo "--- El Torito ---"
xorriso -indev "${OUT_ISO}" -report_el_torito plain 2>&1 | grep -Ei 'El Torito|boot|efi' | head -20 || true

SIZE_BYTES="$(stat -c%s "${OUT_ISO}")"
SHA="$(sha256sum "${OUT_ISO}" | awk '{print $1}')"
echo "ISO_PATH=${OUT_ISO}"
echo "ISO_SIZE_BYTES=${SIZE_BYTES}"
echo "ISO_SIZE_HUMAN=$(numfmt --to=iec "${SIZE_BYTES}" 2>/dev/null || echo "${SIZE_BYTES}")"
echo "ISO_SHA256=${SHA}"
echo "${SHA}  $(basename "${OUT_ISO}")" > "${OUT_ISO}.sha256"
echo "BUILD_DEVELOPER_ISO_OK"
