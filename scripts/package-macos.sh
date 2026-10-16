#!/bin/sh
set -eu

cd "$(dirname "$0")/.."
cargo build --release --locked
bundle="target/release/UniDirStat.app"
mkdir -p "$bundle/Contents/MacOS"
cp target/release/unidirstat "$bundle/Contents/MacOS/unidirstat"
cp packaging/macos/Info.plist "$bundle/Contents/Info.plist"
plutil -lint "$bundle/Contents/Info.plist"
codesign --force --deep --sign - "$bundle"
test -x "$bundle/Contents/MacOS/unidirstat"
printf '%s\n' "$bundle"
