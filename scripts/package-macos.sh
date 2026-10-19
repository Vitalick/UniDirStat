#!/bin/sh
set -eu

cd "$(dirname "$0")/.."
[ "$(uname -s)" = Darwin ] || { printf '%s\n' 'Build the macOS package on macOS.' >&2; exit 1; }
root="$(pwd)"
target="${1:-}"
host="$(rustc -vV | sed -n 's/^host: //p')"
case "${target:-$host}" in
    aarch64-apple-darwin) architecture=arm64 ;;
    x86_64-apple-darwin) architecture=x86_64 ;;
    *) printf '%s\n' 'Expected an Apple arm64 or x86_64 target.' >&2; exit 1 ;;
esac
export MACOSX_DEPLOYMENT_TARGET=11.0
if [ -n "$target" ]; then
    cargo build --release --locked --target "$target"
    binary="$root/target/$target/release/unidirstat"
else
    cargo build --release --locked
    binary="$root/target/release/unidirstat"
fi
version="$(cargo metadata --no-deps --locked --format-version 1 | python3 -c 'import json,sys; print(json.load(sys.stdin)["packages"][0]["version"])')"
numeric_version="${version%%-*}"
numeric_version="${numeric_version%%+*}"
mkdir -p target/packages target/packaging
staging="$(mktemp -d "$root/target/packaging/macos-$architecture.XXXXXX")"
bundle="$staging/disk/UniDirStat.app"
mkdir -p "$bundle/Contents/MacOS" "$bundle/Contents/Resources"
cp "$binary" "$bundle/Contents/MacOS/unidirstat"
cp packaging/macos/Info.plist "$bundle/Contents/Info.plist"
cp assets/unidirstat.icns "$bundle/Contents/Resources/unidirstat.icns"
cp LICENSE "$bundle/Contents/Resources/LICENSE.txt"
cp NOTICE "$bundle/Contents/Resources/NOTICE.txt"
cp assets/fonts/Ubuntu-LICENSE.txt "$bundle/Contents/Resources/Ubuntu-LICENSE.txt"
/usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $numeric_version" "$bundle/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleVersion $numeric_version" "$bundle/Contents/Info.plist"
plutil -lint "$bundle/Contents/Info.plist"
test "$(lipo -archs "$bundle/Contents/MacOS/unidirstat")" = "$architecture"
codesign --force --sign - --timestamp=none "$bundle"
codesign --verify --deep --strict "$bundle"
test -x "$bundle/Contents/MacOS/unidirstat"
"$bundle/Contents/MacOS/unidirstat" --help >/dev/null
ln -s /Applications "$staging/disk/Applications"
cp assets/unidirstat.icns "$staging/disk/.VolumeIcon.icns"
image="$root/target/packages/UniDirStat-$version-macos-$architecture.dmg"
hdiutil create -volname UniDirStat -srcfolder "$staging/disk" -fs HFS+ -format UDRW "$staging/writable.dmg"
mkdir "$staging/mount"
hdiutil attach -nobrowse -noautoopen -mountpoint "$staging/mount" "$staging/writable.dmg" > "$staging/mount.log"
trap 'hdiutil detach "$staging/mount" >/dev/null' EXIT HUP INT TERM
SetFile -a C "$staging/mount"
hdiutil detach "$staging/mount" >/dev/null
trap - EXIT HUP INT TERM
hdiutil convert "$staging/writable.dmg" -format UDZO -o "$image" -ov
hdiutil verify "$image"
hdiutil attach -readonly -nobrowse -noautoopen -mountpoint "$staging/mount" "$image" > "$staging/mount.log"
trap 'hdiutil detach "$staging/mount" >/dev/null' EXIT HUP INT TERM
GetFileInfo -a "$staging/mount" | grep -q C
test "$(readlink "$staging/mount/Applications")" = /Applications
cmp assets/unidirstat.icns "$staging/mount/.VolumeIcon.icns"
cmp assets/unidirstat.icns "$staging/mount/UniDirStat.app/Contents/Resources/unidirstat.icns"
cmp LICENSE "$staging/mount/UniDirStat.app/Contents/Resources/LICENSE.txt"
cmp NOTICE "$staging/mount/UniDirStat.app/Contents/Resources/NOTICE.txt"
cmp assets/fonts/Ubuntu-LICENSE.txt "$staging/mount/UniDirStat.app/Contents/Resources/Ubuntu-LICENSE.txt"
codesign --verify --deep --strict "$staging/mount/UniDirStat.app"
"$staging/mount/UniDirStat.app/Contents/MacOS/unidirstat" --help >/dev/null
hdiutil detach "$staging/mount" >/dev/null
trap - EXIT HUP INT TERM
printf '%s\n' "$bundle" "$image"
