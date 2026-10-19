#!/bin/sh
set -eu

cd "$(dirname "$0")/.."
[ "$(uname -s)" = Linux ] || { printf '%s\n' 'Build the Linux packages on Linux.' >&2; exit 1; }
root="$(pwd)"
target="${1:-x86_64-unknown-linux-gnu}"
[ "$target" = x86_64-unknown-linux-gnu ] || { printf '%s\n' 'Expected the Linux x86_64 GNU target.' >&2; exit 1; }
linuxdeploy="${LINUXDEPLOY:-$(command -v linuxdeploy || true)}"
[ -x "$linuxdeploy" ] || { printf '%s\n' 'Set LINUXDEPLOY to the linuxdeploy AppImage executable.' >&2; exit 1; }
cargo build --release --locked --target "$target"
binary="$root/target/$target/release/unidirstat"
version="$(cargo metadata --no-deps --locked --format-version 1 | python3 -c 'import json,sys; print(json.load(sys.stdin)["packages"][0]["version"])')"
debian_version="$(printf '%s' "$version" | sed 's/-/~/')"
mkdir -p target/packages target/packaging
staging="$(mktemp -d "$root/target/packaging/linux-x86_64.XXXXXX")"
package="$staging/deb"
mkdir -p "$package/DEBIAN" "$package/usr/bin" "$package/usr/share/applications" \
    "$package/usr/share/icons/hicolor/256x256/apps" "$package/usr/share/icons/hicolor/scalable/apps" \
    "$package/usr/share/doc/unidirstat"
install -m 755 "$binary" "$package/usr/bin/unidirstat"
install -m 644 packaging/linux/unidirstat.desktop "$package/usr/share/applications/unidirstat.desktop"
install -m 644 assets/unidirstat.png "$package/usr/share/icons/hicolor/256x256/apps/unidirstat.png"
install -m 644 assets/unidirstat.svg "$package/usr/share/icons/hicolor/scalable/apps/unidirstat.svg"
install -m 644 LICENSE "$package/usr/share/doc/unidirstat/copyright"
install -m 644 NOTICE "$package/usr/share/doc/unidirstat/NOTICE"
install -m 644 assets/fonts/Ubuntu-LICENSE.txt "$package/usr/share/doc/unidirstat/Ubuntu-LICENSE.txt"
desktop-file-validate "$package/usr/share/applications/unidirstat.desktop"
cat > "$package/DEBIAN/control" <<EOF
Package: unidirstat
Version: $debian_version
Section: utils
Priority: optional
Architecture: amd64
Maintainer: UniDirStat contributors
Depends: libc6 (>= 2.35), libgcc-s1, libx11-6, libxkbcommon0, libwayland-client0, libgl1
Recommends: xdg-desktop-portal
Installed-Size: $(du -sk "$package/usr" | cut -f1)
Description: Cross-platform disk usage analyzer
 Explore files and folders with disk usage maps and searchable reports.
EOF
deb="$root/target/packages/UniDirStat-$version-linux-x86_64.deb"
dpkg-deb --root-owner-group --build "$package" "$deb"
dpkg-deb --info "$deb"
dpkg-deb --contents "$deb"
dpkg-deb --extract "$deb" "$staging/installed"
"$staging/installed/usr/bin/unidirstat" --help >/dev/null
appdir="$staging/UniDirStat.AppDir"
mkdir -p "$appdir"
cp -R "$package/usr" "$appdir/usr"
image="$root/target/packages/UniDirStat-$version-linux-x86_64.AppImage"
APPIMAGE_EXTRACT_AND_RUN=1 ARCH=x86_64 LDAI_VERSION="$version" LDAI_OUTPUT="$image" \
    "$linuxdeploy" --appdir "$appdir" --executable "$binary" \
    --desktop-file packaging/linux/unidirstat.desktop --icon-file assets/unidirstat.png \
    --library /usr/lib/x86_64-linux-gnu/libxkbcommon.so.0 --output appimage
test -s "$image"
chmod +x "$image"
mkdir "$staging/check"
(cd "$staging/check" && "$image" --appimage-extract >/dev/null && ./squashfs-root/AppRun --help >/dev/null)
printf '%s\n' "$image" "$deb"
