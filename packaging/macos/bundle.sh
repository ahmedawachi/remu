#!/usr/bin/env bash
#
# Assembles Remu.app, the disk image a Mac user downloads, and the tarball
# whoever hosts the relay downloads.
#
#   packaging/macos/bundle.sh            package binaries that are already built
#   packaging/macos/bundle.sh --build    compile both architectures first
#
# One universal binary rather than two downloads: an office has both Apple
# silicon and Intel Macs, and an asset that runs on half of them is not an
# asset. Release artefacts land in dist/, which is all CI uploads; the bundle
# itself is left in target/macos/Remu.app so it can be opened locally.
#
# Signing is decided by the environment, never by a flag, so a fork holding no
# secrets still produces a download that runs:
#
#   MACOS_CERTIFICATE           base64 .p12, imported into a throwaway keychain
#   MACOS_CERTIFICATE_PASSWORD  its password
#   MACOS_SIGNING_IDENTITY      what to sign with; when unset it is taken from
#                               the imported certificate
#   APPLE_ID + APPLE_TEAM_ID + APPLE_APP_PASSWORD
#                               notarise with Apple and staple the ticket
#
# With none of them set everything is ad-hoc signed. That is not a trust claim
# and Gatekeeper still stops the first launch, but it is what makes the binary
# executable at all on Apple silicon, where the kernel refuses to run unsigned
# code, and it gives TCC one identity to hang the Screen Recording and
# Accessibility grants on instead of re-asking after every rebuild.
set -euo pipefail

cd "$(dirname "$0")/../.."

readonly ARM=aarch64-apple-darwin
readonly INTEL=x86_64-apple-darwin
readonly BUNDLE_ID=dev.remu.desk
readonly RELAY_ID=dev.remu.relay
# ScreenCaptureKit, which the capture backend is built on, arrived in 12.3.
readonly MIN_MACOS=12.3
readonly APP=target/macos/Remu.app
readonly DIST=dist

step() { printf '\n\033[1;34m==> %s\033[0m\n' "$1"; }
note() { printf '    %s\n' "$1"; }
die() { printf '\033[1;31merror:\033[0m %s\n' "$1" >&2; exit 1; }

WORK=$(mktemp -d)
KEYCHAIN=
KEYCHAIN_LIST=
cleanup() {
  # The signing keychain is put *in front of* the user's search list, so a run
  # that dies between import and delete would otherwise leave a developer's
  # login keychain out of the list for the rest of the session.
  if [ -n "$KEYCHAIN_LIST" ]; then
    # Intentionally unquoted: the saved list is several paths.
    # shellcheck disable=SC2086
    security list-keychains -d user -s $KEYCHAIN_LIST >/dev/null 2>&1 || true
  fi
  [ -n "$KEYCHAIN" ] && security delete-keychain "$KEYCHAIN" 2>/dev/null
  rm -rf "$WORK"
  return 0
}
trap cleanup EXIT

# --- version -----------------------------------------------------------------

MANIFEST_VERSION=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -1)
[ -n "$MANIFEST_VERSION" ] || die "no version found in Cargo.toml"
VERSION=${VERSION:-$MANIFEST_VERSION}
# A dry run asks for 0.1.0-dev.1a2b3c4, which CFBundleShortVersionString may not
# hold: LaunchServices compares those two keys numerically and a suffix makes
# them unorderable. The suffix stays in the file names, where it is the point.
PLIST_VERSION=${VERSION%%-*}

# --- compile -------------------------------------------------------------

case "${1:-}" in
  --build)
    step "Compiling for $ARM and $INTEL"
    command -v rustup >/dev/null || die "rustup is needed to add the second Apple target"
    # Adds them to the toolchain rust-toolchain.toml pins, not to whatever
    # stable the machine also has, because that pin is what CI compiled with.
    rustup target add "$ARM" "$INTEL"
    # Exported here rather than trusted to the caller: without it rustc records
    # macOS 11.0 for arm64 and 10.12 for x86_64, and a binary that weak-links
    # ScreenCaptureKit while claiming to run on 10.12 launches on Macs that
    # cannot capture anything and crashes in a framework call instead.
    export MACOSX_DEPLOYMENT_TARGET=$MIN_MACOS
    # Cargo does not re-link a unit it considers fresh, so a binary left at
    # these paths by a build with a different deployment target outlives a
    # rebuild. Deleting them first costs nothing when the right artefact is
    # already in the fingerprint cache — it is re-linked in under a second.
    rm -f "target/$ARM/release/remu" "target/$ARM/release/remu-relay" \
      "target/$INTEL/release/remu" "target/$INTEL/release/remu-relay"
    cargo build --release --target "$ARM" -p remu-desk -p remu-relay
    cargo build --release --target "$INTEL" -p remu-desk -p remu-relay
    ;;
  "") ;;
  *) die "unknown argument: $1" ;;
esac

step "Checking the binaries"
for target in "$ARM" "$INTEL"; do
  for bin in remu remu-relay; do
    path=target/$target/release/$bin
    [ -x "$path" ] || die "$path is missing — run with --build"
    # The floor is only as good as the build that produced it, and nothing
    # downstream can tell. A build without MACOSX_DEPLOYMENT_TARGET leaves a
    # binary right here claiming 11.0 on arm64 or 10.12 on x86_64, and cargo
    # keeps both fingerprints: whichever build ran last owns the hard link, so
    # the wrong one can reappear without recompiling anything.
    # Two load commands to read, because a floor under 10.14 is recorded as
    # LC_VERSION_MIN_MACOSX, whose field is `version`, not `minos`.
    minos=$(vtool -show-build "$path" |
      awk '/minos|version [0-9]+\.[0-9]+/ { print $2; exit }')
    [ "$minos" = "$MIN_MACOS" ] ||
      die "$path targets macOS $minos, not $MIN_MACOS — rebuild it with --build"
  done
done
note "both architectures present, both floored at macOS $MIN_MACOS"

# --- icon --------------------------------------------------------------------

step "Cutting the icon set"
ICON=packaging/icons/icon-1024.png
[ -f "$ICON" ] || ICON=docs/media/mark.png
[ -f "$ICON" ] || die "no icon source: expected packaging/icons/icon-1024.png"
ICON_PX=$(sips -g pixelWidth "$ICON" | awk '/pixelWidth/ { print $2 }')
[ "${ICON_PX:-0}" -ge 128 ] || die "$ICON is only ${ICON_PX}px wide"

ICONSET=$WORK/Remu.iconset
mkdir -p "$ICONSET"
# The names are iconutil's contract, not a convention, and each @2x entry is
# the next size up — which is why a single square master fills every slot.
# Entries larger than the master are skipped rather than upscaled: macOS
# scaling its own 256 up looks no worse than sips scaling it up here, and
# shipping an invented 1024 claims detail the artwork does not have.
ICON_NAMES=(16x16 16x16@2x 32x32 32x32@2x 128x128 128x128@2x 256x256 256x256@2x 512x512 512x512@2x)
ICON_PIXELS=(16 32 32 64 128 256 256 512 512 1024)
skipped=
for i in "${!ICON_NAMES[@]}"; do
  px=${ICON_PIXELS[$i]}
  if [ "$px" -gt "$ICON_PX" ]; then
    skipped="$skipped ${ICON_NAMES[$i]}"
    continue
  fi
  sips -z "$px" "$px" "$ICON" --out "$ICONSET/icon_${ICON_NAMES[$i]}.png" >/dev/null
done
note "from $ICON (${ICON_PX}px)"
[ -n "$skipped" ] && note "no master for:$skipped — macOS will scale the largest entry"

# --- bundle ------------------------------------------------------------------

step "Assembling $APP"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources" "$DIST"

lipo -create -output "$APP/Contents/MacOS/remu" \
  "target/$ARM/release/remu" "target/$INTEL/release/remu"
chmod +x "$APP/Contents/MacOS/remu"

iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/Remu.icns"

cp packaging/macos/Info.plist "$APP/Contents/Info.plist"
# plutil edits the copy by key, rather than sed replacing a placeholder in the
# source: the committed plist stays a real, lintable plist that opens in Xcode,
# and a hand-assembled bundle carries a stale version instead of the literal
# string "@VERSION@".
plutil -replace CFBundleShortVersionString -string "$PLIST_VERSION" "$APP/Contents/Info.plist"
plutil -replace CFBundleVersion -string "$PLIST_VERSION" "$APP/Contents/Info.plist"
plutil -lint "$APP/Contents/Info.plist" >/dev/null || die "Info.plist is not valid"
# Vestigial, and eight bytes: Xcode still writes it, and a bundle that differs
# from the ones LaunchServices sees a billion of is not worth the curiosity.
printf 'APPL????' > "$APP/Contents/PkgInfo"

RELAY=$WORK/remu-relay-$VERSION-macos-universal
mkdir -p "$RELAY"
lipo -create -output "$RELAY/remu-relay" \
  "target/$ARM/release/remu-relay" "target/$INTEL/release/remu-relay"
chmod +x "$RELAY/remu-relay"
cp LICENSE-MIT LICENSE-APACHE "$RELAY/"

note "$(lipo -archs "$APP/Contents/MacOS/remu")"

# --- signing -----------------------------------------------------------------

IDENTITY=${MACOS_SIGNING_IDENTITY:-}

if [ -n "${MACOS_CERTIFICATE:-}" ]; then
  step "Importing the signing certificate"
  KEYCHAIN=$WORK/remu-signing.keychain-db
  password=$(uuidgen)
  security create-keychain -p "$password" "$KEYCHAIN"
  security unlock-keychain -p "$password" "$KEYCHAIN"
  # Six hours, because the default relocks after five minutes and notarisation
  # alone can outlast that.
  security set-keychain-settings -lut 21600 "$KEYCHAIN"
  KEYCHAIN_LIST=$(security list-keychains -d user | tr -d '" ' | tr '\n' ' ')
  # -s replaces the whole search list, so the existing entries have to be
  # named again or codesign loses the login keychain.
  # shellcheck disable=SC2086
  security list-keychains -d user -s "$KEYCHAIN" $KEYCHAIN_LIST
  printf '%s' "$MACOS_CERTIFICATE" | base64 --decode > "$WORK/certificate.p12"
  # -x: the private key may be used for signing but never exported again.
  security import "$WORK/certificate.p12" -k "$KEYCHAIN" \
    -P "${MACOS_CERTIFICATE_PASSWORD:-}" -T /usr/bin/codesign -x
  rm -f "$WORK/certificate.p12"
  # Without this codesign raises a UI prompt for keychain access, which on a
  # runner is a job that hangs until the timeout rather than a job that fails.
  security set-key-partition-list -S apple-tool:,apple:,codesign: \
    -s -k "$password" "$KEYCHAIN" >/dev/null
  if [ -z "$IDENTITY" ]; then
    # By SHA-1 rather than by name: two certificates can share a common name,
    # and a hash cannot be ambiguous.
    IDENTITY=$(security find-identity -v -p codesigning "$KEYCHAIN" |
      awk '$2 ~ /^[0-9A-F]{40}$/ { print $2; exit }')
    [ -n "$IDENTITY" ] || die "the imported certificate holds no code-signing identity"
  fi
  note "identity $IDENTITY"
fi

step "Signing"
if [ -n "$IDENTITY" ]; then
  # --options runtime is not optional here: notarisation rejects a Developer ID
  # signature without the hardened runtime. No entitlements go with it — the
  # binary loads no plug-ins and needs no JIT, and neither Screen Recording nor
  # Accessibility is an entitlement. Both are runtime consent, granted by the
  # user to this bundle identifier. --timestamp is what keeps the signature
  # valid after the certificate expires.
  # No --deep: Apple's own guidance is not to sign with it, and there is nothing
  # nested to reach anyway — the executable links system frameworks only.
  codesign --force --timestamp --options runtime \
    --identifier "$BUNDLE_ID" --sign "$IDENTITY" "$APP"
  codesign --force --timestamp --options runtime \
    --identifier "$RELAY_ID" --sign "$IDENTITY" "$RELAY/remu-relay"
else
  note "no signing identity: ad-hoc signing, and Gatekeeper will stop the first launch"
  # --timestamp=none because an ad-hoc signature has no certificate for a
  # timestamp authority to countersign.
  codesign --force --timestamp=none --identifier "$BUNDLE_ID" --sign - "$APP"
  codesign --force --timestamp=none --identifier "$RELAY_ID" --sign - "$RELAY/remu-relay"
fi

codesign --verify --strict --verbose=2 "$APP"
codesign --verify --strict "$RELAY/remu-relay"
# The one thing that can be executed on this machine without a display: proves
# the universal binary loads and that the signature it just got is accepted by
# the kernel.
"$RELAY/remu-relay" --help >/dev/null || die "the signed relay binary will not run"

# --- notarisation ------------------------------------------------------------

notarise() {
  xcrun notarytool submit "$1" \
    --apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" \
    --password "$APPLE_APP_PASSWORD" --wait --timeout 30m ||
    die "notarisation was rejected; 'xcrun notarytool log <the id above>' says why"
}

NOTARISE=false
if [ -n "$IDENTITY" ] && [ -n "${APPLE_ID:-}" ] &&
  [ -n "${APPLE_TEAM_ID:-}" ] && [ -n "${APPLE_APP_PASSWORD:-}" ]; then
  NOTARISE=true
  command -v xcrun >/dev/null || die "notarisation needs the Xcode command line tools"
elif [ -n "$IDENTITY" ]; then
  note "signed but not notarised: Gatekeeper will still refuse the first launch"
fi

if [ "$NOTARISE" = true ]; then
  step "Notarising Remu.app"
  # notarytool takes a zip, a dmg or a pkg, never a bare bundle. ditto is what
  # zips a signed bundle without breaking the signature.
  ditto -c -k --keepParent "$APP" "$WORK/Remu.zip"
  notarise "$WORK/Remu.zip"
  # Stapled to the app itself, not only to the disk image: the app is what gets
  # dragged out and launched, and without its own ticket a first launch with no
  # network cannot be verified. Nothing may modify the bundle after this — the
  # ticket sits outside the signature, and re-signing discards it.
  xcrun stapler staple "$APP"
fi

# --- disk image --------------------------------------------------------------

step "Building the disk image"
DMG=$DIST/Remu-$VERSION-macos-universal.dmg
STAGE=$WORK/stage
mkdir -p "$STAGE"
# ditto, not cp -R: it is the one copy that is documented to preserve a signed
# bundle whole, down to the extended attributes.
ditto "$APP" "$STAGE/Remu.app"
ditto "$RELAY/remu-relay" "$STAGE/remu-relay"
cp packaging/macos/README.txt "$STAGE/Read Me.txt"
# The drag target. This is why a disk image rather than a zip: unzipping leaves
# the app in Downloads, and both permissions Remu needs are remembered against
# where the app was when they were granted.
ln -s /Applications "$STAGE/Applications"
rm -f "$DMG"
hdiutil create -volname Remu -srcfolder "$STAGE" -fs HFS+ \
  -format UDZO -imagekey zlib-level=9 -ov "$DMG" >/dev/null

if [ "$NOTARISE" = true ]; then
  step "Notarising the disk image"
  # A second submission, for the file the user actually downloads: the ticket
  # stapled to the app inside is invisible to Gatekeeper's check on the image.
  notarise "$DMG"
  xcrun stapler staple "$DMG"
fi

step "Packing the relay"
TARBALL=$DIST/remu-relay-$VERSION-macos-universal.tar.gz
# COPYFILE_DISABLE: macOS tar otherwise writes an AppleDouble ._remu-relay
# member to carry extended attributes. The signature is inside the Mach-O, so
# there is nothing there worth the clutter.
# One directory inside the archive, as the Linux and Windows archives have, so
# extracting in Downloads does not scatter a binary and two licences across it.
COPYFILE_DISABLE=1 tar -C "$WORK" -czf "$TARBALL" "$(basename "$RELAY")"

# --- what came out of it -----------------------------------------------------

step "Done"
ls -lh "$DMG" "$TARBALL"
note "bundle: $APP"
if [ "$NOTARISE" = true ]; then
  xcrun stapler validate "$DMG" >/dev/null &&
    note "notarised and stapled: opens with no warning"
else
  # Reported rather than asserted: an ad-hoc signature is always rejected here,
  # and that is the expected outcome, not a failure.
  note "Gatekeeper: $(spctl --assess --type execute "$APP" 2>&1 | tail -1)"
  cat <<'FIRSTRUN'

    Unsigned build. On the downloading Mac, first open goes:
      1. open the .dmg, drag Remu to Applications
      2. double-click Remu — macOS refuses, offering only Done or Move to Trash
      3. System Settings > Privacy & Security > "Open Anyway" (Touch ID), then
         Open Anyway once more
    Control-clicking Open has not been a way around this since macOS 15.
    The one-line alternative: xattr -dr com.apple.quarantine /Applications/Remu.app
FIRSTRUN
fi
