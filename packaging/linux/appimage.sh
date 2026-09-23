#!/usr/bin/env bash
#
# Builds the Linux release artifacts:
#
#   Remu-<version>-<arch>.AppImage        the desk app, one file, double-clickable
#   remu-<version>-<arch>-linux.tar.gz    the same payload for machines without FUSE
#   SHA256SUMS-linux-<arch>.txt
#
# Run it on the oldest glibc the release supports — the release workflow does
# that on an ubuntu-24.04 runner — not on whatever machine is handy. A
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
# Outside WORK_DIR, which is wiped every run: the three downloads below are
# 36 MB together, and are re-verified against their digests whether they were
# just fetched or not.
TOOLS="$REPO_ROOT/target/appimage-tools"

# The highest glibc symbol version the binaries may require. 2.39 is Ubuntu
# 24.04, which also clears Debian 13, Fedora 40+, Mint 22+ and Arch. It is
# not lower because it cannot be: scap's libspa 0.8 bindings need PipeWire
# headers newer than Ubuntu 22.04 ships, so nothing older builds this graph.
# Raise it only together with a decision about who stops being able to run it.
GLIBC_MAX="${GLIBC_MAX:-2.39}"
# The same floor for libstdc++, which the host provides too: 3.4.32 is GCC 13,
# Ubuntu 24.04's default compiler, and every distribution at the glibc floor
# ships at least that runtime.
GLIBCXX_MAX="${GLIBCXX_MAX:-3.4.32}"

# linuxdeploy, its AppImage plugin and the AppImage runtime all publish a
# rolling "continuous" tag as well as dated ones. The dated tags are used here
# because a release built next month has to be the same bundle as one built
# today, and the digests are what those assets were when they were pinned. A
# mismatch means upstream re-cut the tag: look at the new file before moving
# the pin.
#
# The runtime is pinned separately because nothing else pins it: left alone,
# the appimagetool inside the plugin downloads the rolling runtime on every
# build and checks no digest. It is the ELF header of the AppImage — the first
# code that runs on the user's machine, and the code that decides whether the
# no-FUSE fallback the release notes promise works at all.
LINUXDEPLOY_TAG="${LINUXDEPLOY_TAG:-1-alpha-20251107-1}"
LINUXDEPLOY_PLUGIN_TAG="${LINUXDEPLOY_PLUGIN_TAG:-1-alpha-20250213-1}"
APPIMAGE_RUNTIME_TAG="${APPIMAGE_RUNTIME_TAG:-20251108}"
case "$ARCH" in
    x86_64)
        LINUXDEPLOY_SHA256="${LINUXDEPLOY_SHA256:-c20cd71e3a4e3b80c3483cef793cda3f4e990aca14014d23c544ca3ce1270b4d}"
        LINUXDEPLOY_PLUGIN_SHA256="${LINUXDEPLOY_PLUGIN_SHA256:-992d502a248e14ab185448ddf6f6e7d25558cb84d4623c354c3af350c25fccb3}"
        APPIMAGE_RUNTIME_SHA256="${APPIMAGE_RUNTIME_SHA256:-2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d}"
        ;;
    *)
        # The same tags carry other architectures, but nobody has verified
        # those assets here. Supplying the digests is the sign-off.
        if [ -z "${LINUXDEPLOY_SHA256:-}" ] || [ -z "${LINUXDEPLOY_PLUGIN_SHA256:-}" ] ||
            [ -z "${APPIMAGE_RUNTIME_SHA256:-}" ]; then
            die "$ARCH is not pinned: export LINUXDEPLOY_SHA256, LINUXDEPLOY_PLUGIN_SHA256 and APPIMAGE_RUNTIME_SHA256 for the $ARCH assets first"
        fi
        ;;
esac

for tool in curl sha256sum objdump patchelf tar; do
    command -v "$tool" >/dev/null 2>&1 || die "$tool is not installed"
done

# ----------------------------------------------------------------- build -----

if [ "${SKIP_BUILD:-0}" != "1" ]; then
    say "Building remu and remu-relay (release)"
    cargo build --release -p remu-desk -p remu-relay
fi

for binary in target/release/remu target/release/remu-relay; do
    [ -x "$binary" ] || die "$binary is missing; run without SKIP_BUILD=1"
done

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

# libstdc++ is on that list for the same reason glibc is, and it is the one
# worth spelling out, because openh264 is C++ and linuxdeploy would otherwise
# bundle it. A bundled libstdc++.so.6 is handed to every library the host
# loads into this process — the GL driver included — and a driver built
# against a newer GLIBCXX then fails to load and the app drops to software
# rendering. Linking it statically is not the answer either: `cc` emits an
# explicit `-lstdc++`, which `-static-libstdc++` does not touch, and asking cc
# for a static link makes rustc bundle libstdc++.a into an rlib it cannot find
# it for. Every desktop has libstdc++.so.6; what varies is its version, so the
# floor is checked below exactly as glibc's is.
#
# The last two lines are not anything Remu uses. linuxdeploy works from ldd's
# flattened list, so excluding a library does not exclude what that library
# pulls in: libbsd and libmd arrive under libXdmcp, and libcap, liblz4, liblzma
# and libzstd under libsystemd — both excluded above. Shipping those children
# would hand the host's own libsystemd our copies of its dependencies, which is
# the same shadowing the rest of this list exists to prevent, one level down.
# The allow list below is what catches the next one of these.
#
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
    'libc.so*' 'libm.so*' 'libdl.so*' 'libpthread.so*' 'librt.so*' 'libgcc_s.so*' 'ld-linux*.so*' \
    'libstdc++.so*' \
    'libbsd.so*' 'libmd.so*' \
    'libcap.so*' 'liblz4.so*' 'liblzma.so*' 'libzstd.so*'
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
    # Not a warning: shipping the bundled libxkbcommon without its X11 half
    # makes winit dlopen the user's libxkbcommon-x11, which then binds to our
    # libxkbcommon — two builds of one library in one process.
    die "libxkbcommon-x11.so.0 is not installed here, and the bundle must carry it beside libxkbcommon.
Debian/Ubuntu: apt install libxkbcommon-x11-dev"
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
# Fetched on its own because it is run on its own, after the checks below, to
# seal the AppImage. Going through linuxdeploy again for that step would
# re-deploy every dependency of the AppDir without the exclude list.
fetch_tool \
    "https://github.com/linuxdeploy/linuxdeploy-plugin-appimage/releases/download/$LINUXDEPLOY_PLUGIN_TAG/linuxdeploy-plugin-appimage-$ARCH.AppImage" \
    "$TOOLS/linuxdeploy-plugin-appimage-$ARCH.AppImage" "$LINUXDEPLOY_PLUGIN_SHA256"
fetch_tool \
    "https://github.com/AppImage/type2-runtime/releases/download/$APPIMAGE_RUNTIME_TAG/runtime-$ARCH" \
    "$TOOLS/runtime-$ARCH" "$APPIMAGE_RUNTIME_SHA256"

# Both tools are AppImages themselves and a CI runner has no /dev/fuse, so
# they have to unpack rather than mount.
export APPIMAGE_EXTRACT_AND_RUN=1

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

denied='^(libGL|libGLX|libEGL|libGLdispatch|libOpenGL|libvulkan|libdrm|libgbm|libglapi|libX11|libX[a-zA-Z0-9]|libxcb|libwayland|libpipewire|libspa|libdbus-1|libsystemd|libc\.so|libm\.so|libdl\.so|libpthread\.so|librt\.so|libgcc_s|ld-linux|libstdc\+\+|libbsd|libmd\.so|libcap\.so|liblz4|liblzma|libzstd)'
allowed='^(libxkbcommon\.so|libxkbcommon-x11\.so)'

# Run against the AppDir before sealing, and again against what each artefact
# actually carries, so the check describes the files that ship rather than an
# intermediate state a later step could still change.
check_bundled_libs() {
    local dir="$1" what="$2" lib base
    [ -d "$dir" ] || return 0
    while IFS= read -r lib; do
        base="$(basename "$lib")"
        if printf '%s' "$base" | grep -Eq "$denied"; then
            die "$what carries $base, which must not be bundled: it has to come from the user's system"
        fi
        if ! printf '%s' "$base" | grep -Eq "$allowed"; then
            die "$what carries $base, and nobody decided that. Exclude it, or add it to the allow list with a reason."
        fi
        printf '  %s: %s\n' "$what" "$base"
    done < <(find "$dir" -maxdepth 1 \( -type f -o -type l \) | sort)
}
check_bundled_libs "$APPDIR/usr/lib" "AppDir"

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
Build on Ubuntu 24.04, as the release workflow does, or raise GLIBC_MAX
deliberately and say in the release notes which distributions just lost support."
fi

cxxfloor=""
for binary in "$APPDIR/usr/bin/remu" target/release/remu-relay; do
    highest="$(objdump -p "$binary" | grep -oE 'GLIBCXX_[0-9]+(\.[0-9]+)+' | sed 's/GLIBCXX_//' | sort -V | tail -1 || true)"
    [ -n "$highest" ] || continue
    cxxfloor="$(printf '%s\n%s\n' "$cxxfloor" "$highest" | sed '/^$/d' | sort -V | tail -1 || true)"
done
printf '  libstdc++ floor: GLIBCXX_%s (limit GLIBCXX_%s)\n' "${cxxfloor:-none}" "$GLIBCXX_MAX"
if [ -n "$cxxfloor" ] && [ "$(printf '%s\n%s\n' "$cxxfloor" "$GLIBCXX_MAX" | sort -V | tail -1)" != "$GLIBCXX_MAX" ]; then
    die "these binaries need GLIBCXX_$cxxfloor, above the GLIBCXX_$GLIBCXX_MAX this release promises.
The C++ compiler is newer than the release's floor: build with the distribution's
default g++ on Ubuntu 24.04, or raise GLIBCXX_MAX deliberately."
fi

# ---------------------------------------------------------------- output -----

# Sealing is a separate step so that the checks above sit between deployment
# and the sealed file, and it runs the plugin directly rather than through
# linuxdeploy. That is not a shortcut: every linuxdeploy run re-traces every ELF
# already in the AppDir and deploys what it finds, whatever flags it is given,
# so a second linuxdeploy call would re-bundle everything excluded above — after
# the check that exists to catch it. The plugin takes the AppDir as it is: the
# first call already put the desktop entry, icon and AppRun at its root.
say "Building the AppImage"
APPIMAGE="$OUT_DIR/Remu-$VERSION-$ARCH.AppImage"
LDAI_OUTPUT="$APPIMAGE" \
LINUXDEPLOY_OUTPUT_VERSION="$VERSION" \
LDAI_RUNTIME_FILE="$TOOLS/runtime-$ARCH" \
LDAI_NO_APPSTREAM=1 \
    "$TOOLS/linuxdeploy-plugin-appimage-$ARCH.AppImage" --appdir "$APPDIR"
[ -f "$APPIMAGE" ] || die "the plugin finished without writing $APPIMAGE"
chmod +x "$APPIMAGE"

say "Verifying the sealed AppImage"
# Unpacked with the runtime's own extractor, which needs no FUSE, so what is
# checked is the squashfs a user's machine will mount, not the AppDir it was
# made from.
SEALED="$WORK_DIR/sealed"
rm -rf "$SEALED" && mkdir -p "$SEALED"
( cd "$SEALED" && "$APPIMAGE" --appimage-extract >/dev/null )
check_bundled_libs "$SEALED/squashfs-root/usr/lib" "AppImage"
grep -q 'Remu cannot start' "$SEALED/squashfs-root/AppRun" \
    || die "the sealed AppImage does not start through packaging/linux/AppRun"
[ -f "$SEALED/squashfs-root/dev.remu.desk.desktop" ] \
    || die "the sealed AppImage has no dev.remu.desk.desktop at its root"
[ -x "$SEALED/squashfs-root/usr/bin/remu" ] \
    || die "the sealed AppImage has no usr/bin/remu"

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
# The same names the AppImage uses, for the same reason: the entry's Icon= key
# and the window's app id both say dev.remu.desk.
install -m644 "$STAGE/dev.remu.desk.desktop" "$FLAT/dev.remu.desk.desktop"
install -m644 "$STAGE/dev.remu.desk.png" "$FLAT/dev.remu.desk.png"
install -m644 README.md LICENSE-MIT LICENSE-APACHE "$FLAT/"
check_bundled_libs "$FLAT/lib" "tarball"
tar --owner=0 --group=0 --numeric-owner -czf \
    "$OUT_DIR/remu-$VERSION-$ARCH-linux.tar.gz" -C "$WORK_DIR" "remu-$VERSION-$ARCH-linux"

say "Checksums"
( cd "$OUT_DIR" && sha256sum \
    "Remu-$VERSION-$ARCH.AppImage" \
    "remu-$VERSION-$ARCH-linux.tar.gz" | tee "SHA256SUMS-linux-$ARCH.txt" )

printf '\n\033[1;32mLinux artifacts are in %s\033[0m\n' "$OUT_DIR"
