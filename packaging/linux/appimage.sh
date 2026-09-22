#!/usr/bin/env bash
#
# Builds the Linux release artifacts:
#
#   Remu-<version>-<arch>.AppImage        the desk app, one file, double-clickable
#   remu-<version>-<arch>-linux.tar.gz    the same payload for machines without FUSE
#   SHA256SUMS-linux-<arch>.txt
#
# Run it on the oldest glibc the release supports — the release workflow does
# that in an ubuntu:22.04 container — not on whatever machine is handy. A
# binary's glibc floor is whatever it was linked against, and a floor above
# GLIBC_MAX means the user sees "GLIBC_2.39 not found" instead of a window, so
# the check before packaging refuses to ship it.
#
#   ./packaging/linux/appimage.sh
#   VERSION=0.2.0 SKIP_BUILD=1 ./packaging/linux/appimage.sh
#
# Signing is optional and off unless the caller exports LDAI_SIGN=1 with a GPG
# key in the agent; no Linux desktop refuses to run an unsigned download, so an
# unsigned artifact behaves exactly like a signed one.
set -euo pipefail

say()  { printf '\n\033[1;34m==> %s\033[0m\n' "$1"; }
warn() { printf '\033[1;33mwarning: %s\033[0m\n' "$1" >&2; }
die()  { printf '\033[1;31merror: %s\033[0m\n' "$1" >&2; exit 1; }

[ "$(uname -s)" = "Linux" ] || die "this packages Linux artifacts and has to run on Linux"

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$REPO_ROOT"

ARCH="${ARCH:-$(uname -m)}"
VERSION="${VERSION:-$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -1)}"
VERSION="${VERSION#v}"
[ -n "$VERSION" ] || die "could not read the version out of Cargo.toml; pass VERSION=x.y.z"

# Default under target/ so a release build leaves nothing untracked behind in
# the working tree, and never delete this directory: a workflow may have put
# another platform's artifacts here first.
OUT_DIR="${OUT_DIR:-$REPO_ROOT/target/dist}"
WORK_DIR="$REPO_ROOT/target/appimage"
APPDIR="$WORK_DIR/AppDir"
STAGE="$WORK_DIR/stage"
# Outside WORK_DIR, which is wiped every run: these two downloads are 35 MB and
# are re-verified against their digests whether they were just fetched or not.
TOOLS="$REPO_ROOT/target/appimage-tools"

# The highest glibc symbol version the binaries may require. 2.35 is Ubuntu
# 22.04, which also clears Debian 12, Fedora 36+, Mint 21+ and Arch. Raise it
# only together with a decision about who stops being able to run this.
GLIBC_MAX="${GLIBC_MAX:-2.35}"

# linuxdeploy publishes a rolling "continuous" tag as well as dated ones. The
# dated tags are used here because a release built next month has to be the
# same bundle as one built today, and the digests are what those two assets
# were when they were pinned. A mismatch means upstream re-cut the tag: look
# at the new file before moving the pin.
LINUXDEPLOY_TAG="${LINUXDEPLOY_TAG:-1-alpha-20251107-1}"
LINUXDEPLOY_PLUGIN_TAG="${LINUXDEPLOY_PLUGIN_TAG:-1-alpha-20250213-1}"
case "$ARCH" in
    x86_64)
        LINUXDEPLOY_SHA256="${LINUXDEPLOY_SHA256:-c20cd71e3a4e3b80c3483cef793cda3f4e990aca14014d23c544ca3ce1270b4d}"
        LINUXDEPLOY_PLUGIN_SHA256="${LINUXDEPLOY_PLUGIN_SHA256:-992d502a248e14ab185448ddf6f6e7d25558cb84d4623c354c3af350c25fccb3}"
        ;;
    *)
        # The same tags carry other architectures, but nobody has verified
        # those assets here. Supplying the digests is the sign-off.
        if [ -z "${LINUXDEPLOY_SHA256:-}" ] || [ -z "${LINUXDEPLOY_PLUGIN_SHA256:-}" ]; then
            die "$ARCH is not pinned: export LINUXDEPLOY_SHA256 and LINUXDEPLOY_PLUGIN_SHA256 for the $ARCH assets first"
        fi
        ;;
esac

for tool in curl sha256sum objdump patchelf tar; do
    command -v "$tool" >/dev/null 2>&1 || die "$tool is not installed"
done

# ----------------------------------------------------------------- build -----

if [ "${SKIP_BUILD:-0}" != "1" ]; then
    say "Building remu and remu-relay (release)"
    # openh264 is C++ and the only reason libstdc++ is in the picture. It is
    # linked statically rather than bundled: a libstdc++.so.6 inside the
    # AppImage is also handed to every library the host loads into this
    # process, including the GL driver, which then cannot find the newer
    # GLIBCXX it was built against and quietly drops the app to software
    # rendering. libgcc_s stays dynamic on purpose — it is present everywhere,
    # and two unwinders in one process is the worse trade.
    RUSTFLAGS="${RUSTFLAGS:-} -C link-arg=-static-libstdc++" \
        cargo build --release -p remu-desk -p remu-relay
fi

for binary in target/release/remu target/release/remu-relay; do
    [ -x "$binary" ] || die "$binary is missing; run without SKIP_BUILD=1"
done

BUNDLE_LIBSTDCXX="${BUNDLE_LIBSTDCXX:-0}"
if objdump -p target/release/remu | grep -q 'NEEDED.*libstdc++'; then
    if [ "$BUNDLE_LIBSTDCXX" != "1" ]; then
        die "libstdc++ is still a dynamic dependency, so -static-libstdc++ did not take.
Install the C++ toolchain's static library (on Debian/Ubuntu it ships with g++),
or accept the shadowing risk explicitly with BUNDLE_LIBSTDCXX=1."
    fi
    warn "bundling libstdc++.so.6: the host GL driver may fail to load against it"
fi

# -------------------------------------------------------------- assemble -----

say "Assembling the AppDir"
rm -rf "$WORK_DIR"
mkdir -p "$APPDIR/usr/share/doc/remu" "$STAGE" "$TOOLS" "$OUT_DIR"

# The desktop file has to be installed under the Wayland app id the window
# sets (with_app_id in crates/remu-desk/src/main.rs), or the compositor cannot
# tie the window to its launcher and the dock grows a second, iconless entry.
install -m644 packaging/linux/remu.desktop "$STAGE/dev.remu.desk.desktop"
install -m644 docs/media/mark.png "$STAGE/dev.remu.desk.png"

if command -v desktop-file-validate >/dev/null 2>&1; then
    desktop-file-validate "$STAGE/dev.remu.desk.desktop" || die "the desktop file is not valid"
fi

# Libraries the host owns. Bundling any of these is the classic AppImage
# failure: the copy inside the bundle wins for the whole process and then
# disagrees with the kernel driver, the compositor or the host GL stack it has
# to cooperate with. Most are never linked directly — wgpu dlopens Vulkan and
# GL, winit dlopens X11 and xcb — so this list is really about what must not
# arrive through a transitive dependency.
exclude_args=()
for pattern in \
    'libGL*.so*' 'libGLX*.so*' 'libEGL*.so*' 'libGLdispatch*.so*' 'libOpenGL*.so*' \
    'libvulkan*.so*' 'libdrm*.so*' 'libgbm*.so*' 'libglapi*.so*' \
    'libX11*.so*' 'libxcb*.so*' 'libX*.so*' \
    'libwayland*.so*' \
    'libpipewire*.so*' 'libspa*.so*' \
    'libdbus-1*.so*' 'libsystemd*.so*' 'libgcrypt*.so*' 'libgpg-error*.so*' \
    'libfontconfig*.so*' 'libfreetype*.so*' \
    'libc.so*' 'libm.so*' 'libdl.so*' 'libpthread.so*' 'librt.so*' 'libgcc_s.so*' 'ld-linux*.so*'
do
    exclude_args+=(--exclude-library "$pattern")
done

# libxkbcommon is the one exception, bundled for one reason: it is a hard
# NEEDED entry, so its absence is a loader error before a pixel is drawn, and
# unlike everything above it talks to nothing but /usr/share/X11/xkb, which it
# finds by absolute path. Its X11 half is dlopened by winit, so linuxdeploy
# cannot see it and it has to be named here — the pair must come from one
# build or they disagree internally.
library_args=()
xkb_x11="$(ldconfig -p 2>/dev/null | awk '/libxkbcommon-x11\.so\.0/ {print $NF; exit}' || true)"
if [ -n "$xkb_x11" ] && [ -e "$xkb_x11" ]; then
    library_args+=(--library "$xkb_x11")
else
    warn "libxkbcommon-x11.so.0 was not found here; X11 keyboard input will depend on the user's copy"
fi

if [ "$BUNDLE_LIBSTDCXX" = "1" ]; then
    libstdcxx="$(ldconfig -p 2>/dev/null | awk '/libstdc\+\+\.so\.6/ {print $NF; exit}' || true)"
    [ -n "$libstdcxx" ] || die "BUNDLE_LIBSTDCXX=1 but libstdc++.so.6 is not in the loader cache"
    library_args+=(--library "$libstdcxx")
fi

# ----------------------------------------------------------------- tools -----

fetch_tool() {
    local url="$1" dest="$2" digest="$3"
    if [ -f "$dest" ] && printf '%s  %s\n' "$digest" "$dest" | sha256sum --status -c -; then
        say "Reusing $(basename "$dest")"
    else
        say "Fetching $(basename "$dest")"
        curl -fsSL --retry 3 -o "$dest" "$url"
        printf '%s  %s\n' "$digest" "$dest" | sha256sum -c - \
            || die "$(basename "$dest") does not match its pinned digest; inspect the new upstream build before changing the pin"
    fi
    chmod +x "$dest"
}

fetch_tool \
    "https://github.com/linuxdeploy/linuxdeploy/releases/download/$LINUXDEPLOY_TAG/linuxdeploy-$ARCH.AppImage" \
    "$TOOLS/linuxdeploy-$ARCH.AppImage" "$LINUXDEPLOY_SHA256"
# Shipped separately from linuxdeploy, which prefers a plugin found on PATH
# over the older copy inside its own bundle.
fetch_tool \
    "https://github.com/linuxdeploy/linuxdeploy-plugin-appimage/releases/download/$LINUXDEPLOY_PLUGIN_TAG/linuxdeploy-plugin-appimage-$ARCH.AppImage" \
    "$TOOLS/linuxdeploy-plugin-appimage-$ARCH.AppImage" "$LINUXDEPLOY_PLUGIN_SHA256"

# Both tools are AppImages themselves and a CI container has no /dev/fuse, so
# they have to unpack rather than mount.
export APPIMAGE_EXTRACT_AND_RUN=1
export PATH="$TOOLS:$PATH"

# ---------------------------------------------------------------- deploy -----

say "Deploying dependencies into the AppDir"
"$TOOLS/linuxdeploy-$ARCH.AppImage" \
    --appdir "$APPDIR" \
    --executable target/release/remu \
    --desktop-file "$STAGE/dev.remu.desk.desktop" \
    --icon-file "$STAGE/dev.remu.desk.png" \
    --custom-apprun packaging/linux/AppRun \
    "${exclude_args[@]}" \
    ${library_args[@]+"${library_args[@]}"}

chmod +x "$APPDIR/AppRun"
install -m644 README.md LICENSE-MIT LICENSE-APACHE "$APPDIR/usr/share/doc/remu/"

# ---------------------------------------------------------------- verify -----

say "Verifying the bundle before it is sealed"

grep -q 'Remu cannot start' "$APPDIR/AppRun" \
    || die "AppDir/AppRun is not packaging/linux/AppRun; --custom-apprun was ignored and the missing-library message is gone"

denied='^(libGL|libGLX|libEGL|libGLdispatch|libOpenGL|libvulkan|libdrm|libgbm|libglapi|libX11|libX[a-zA-Z0-9]|libxcb|libwayland|libpipewire|libspa|libdbus-1|libsystemd|libc\.so|libm\.so|libdl\.so|libpthread\.so|librt\.so|libgcc_s|ld-linux)'
allowed='^(libxkbcommon\.so|libxkbcommon-x11\.so)'
if [ "$BUNDLE_LIBSTDCXX" = "1" ]; then
    allowed="$allowed"'|^libstdc\+\+\.so'
fi

if [ -d "$APPDIR/usr/lib" ]; then
    while IFS= read -r lib; do
        base="$(basename "$lib")"
        if printf '%s' "$base" | grep -Eq "$denied"; then
            die "$base was bundled and must not be: it has to come from the user's system"
        fi
        if ! printf '%s' "$base" | grep -Eq "$allowed"; then
            die "$base was bundled and nobody decided that. Exclude it, or add it to the allow list with a reason."
        fi
        printf '  bundled: %s\n' "$base"
    done < <(find "$APPDIR/usr/lib" -maxdepth 1 \( -type f -o -type l \) | sort)
fi

printf '\n  host must provide:\n'
objdump -p "$APPDIR/usr/bin/remu" | awk '/NEEDED/ {printf "    %s\n", $2}'

# The version references in the program header survive `strip`, which the
# release profile applies, so they are the honest source for the floor.
floor=""
for binary in "$APPDIR/usr/bin/remu" target/release/remu-relay; do
    highest="$(objdump -p "$binary" | grep -oE 'GLIBC_[0-9]+\.[0-9]+' | sed 's/GLIBC_//' | sort -V | tail -1 || true)"
    [ -n "$highest" ] || continue
    floor="$(printf '%s\n%s\n' "$floor" "$highest" | sed '/^$/d' | sort -V | tail -1 || true)"
done
printf '\n  glibc floor: %s (limit %s)\n' "${floor:-unknown}" "$GLIBC_MAX"
if [ -n "$floor" ] && [ "$(printf '%s\n%s\n' "$floor" "$GLIBC_MAX" | sort -V | tail -1)" != "$GLIBC_MAX" ]; then
    die "these binaries need glibc $floor, above the $GLIBC_MAX this release promises.
Build in the ubuntu:22.04 container the release workflow uses, or raise GLIBC_MAX
deliberately and say in the release notes which distributions just lost support."
fi

# ---------------------------------------------------------------- output -----

# A second invocation, so that the checks above sit between deployment and the
# sealed file. The desktop file, icon and AppRun are passed again because this
# call re-derives them from its arguments; without --executable it deploys no
# libraries, so nothing verified above can change here.
say "Building the AppImage"
APPIMAGE="$OUT_DIR/Remu-$VERSION-$ARCH.AppImage"
LDAI_OUTPUT="$APPIMAGE" \
LDAI_VERSION="$VERSION" \
LDAI_NO_APPSTREAM=1 \
    "$TOOLS/linuxdeploy-$ARCH.AppImage" \
        --appdir "$APPDIR" \
        --desktop-file "$STAGE/dev.remu.desk.desktop" \
        --icon-file "$STAGE/dev.remu.desk.png" \
        --custom-apprun packaging/linux/AppRun \
        --output appimage
chmod +x "$APPIMAGE"

# Every no-FUSE instruction in the release notes goes through this switch in
# the runtime, so if a future runtime drops it the notes are wrong and the user
# is stuck with a file that will not open.
runtime_switch="$(head -c 2000000 "$APPIMAGE" | tr -d '\0' | grep -ac 'APPIMAGE_EXTRACT_AND_RUN' || true)"
if [ "${runtime_switch:-0}" = "0" ]; then
    warn "this AppImage's runtime does not advertise APPIMAGE_EXTRACT_AND_RUN; the no-FUSE fallback in the release notes will not work"
fi

say "Building the portable tarball"
FLAT="$WORK_DIR/remu-$VERSION-$ARCH-linux"
rm -rf "$FLAT"
mkdir -p "$FLAT/lib"
install -m755 "$APPDIR/usr/bin/remu" "$FLAT/remu"
install -m755 target/release/remu-relay "$FLAT/remu-relay"
if [ -d "$APPDIR/usr/lib" ] && [ -n "$(ls -A "$APPDIR/usr/lib")" ]; then
    cp -a "$APPDIR"/usr/lib/. "$FLAT/lib/"
fi
# The AppDir keeps libraries one level up from the binary; this layout is flat,
# because someone who unpacks a tarball should see ./remu, not usr/bin/remu.
patchelf --set-rpath '$ORIGIN/lib' "$FLAT/remu"
install -m644 packaging/linux/remu.desktop "$FLAT/remu.desktop"
install -m644 docs/media/mark.png "$FLAT/remu.png"
install -m644 README.md LICENSE-MIT LICENSE-APACHE "$FLAT/"
tar --owner=0 --group=0 --numeric-owner -czf \
    "$OUT_DIR/remu-$VERSION-$ARCH-linux.tar.gz" -C "$WORK_DIR" "remu-$VERSION-$ARCH-linux"

say "Checksums"
( cd "$OUT_DIR" && sha256sum \
    "Remu-$VERSION-$ARCH.AppImage" \
    "remu-$VERSION-$ARCH-linux.tar.gz" | tee "SHA256SUMS-linux-$ARCH.txt" )

printf '\n\033[1;32mLinux artifacts are in %s\033[0m\n' "$OUT_DIR"
