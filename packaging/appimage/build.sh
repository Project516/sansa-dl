#!/usr/bin/env bash
# Builds dist/clipplus-manager-<version>-x86_64.AppImage and its .sha256.
# The version is $CLIPPLUS_VERSION, or the one in Cargo.toml.
# Qt 6 and Kirigami come from the host, so they are not bundled.
set -euo pipefail

cd "$(dirname "$0")/../.."
root=$PWD
here=$root/packaging/appimage

version=${CLIPPLUS_VERSION:-$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)}
export CLIPPLUS_VERSION=$version

build=$root/build/appimage
appdir=$build/AppDir
rm -rf "$appdir"
mkdir -p "$appdir/usr/bin" "$appdir/usr/lib/clipplus-manager/tools" "$root/dist"

cargo build --release
install -m755 "${CARGO_TARGET_DIR:-target}/release/clipplus-manager" "$appdir/usr/bin/clipplus-manager"

tools=$root/build/tools
[ -f "$tools/versions.json" ] || "$here/fetch-tools.sh" "$tools"
cp -a "$tools"/. "$appdir/usr/lib/clipplus-manager/tools/"

install -m755 "$here/AppRun" "$appdir/AppRun"
install -m644 "$here/dev.project516.clipplus-manager.desktop" "$appdir/dev.project516.clipplus-manager.desktop"
install -m644 data/icons/dev.project516.clipplus-manager.svg "$appdir/dev.project516.clipplus-manager.svg"
ln -sf dev.project516.clipplus-manager.svg "$appdir/.DirIcon"

# appimagetool publishes no checksum file; GitHub records a digest per asset.
github() {
  if [ -n "${GITHUB_TOKEN:-}" ]; then
    curl -fsSL -H "Authorization: Bearer $GITHUB_TOKEN" "$@"
  else
    curl -fsSL "$@"
  fi
}
asset=appimagetool-x86_64.AppImage
meta=$(github https://api.github.com/repos/AppImage/appimagetool/releases/latest)
url=$(jq -r --arg n "$asset" '.assets[] | select(.name == $n) | .browser_download_url' <<<"$meta")
digest=$(jq -r --arg n "$asset" '.assets[] | select(.name == $n) | .digest' <<<"$meta")
digest=${digest#sha256:}
[ -n "$url" ] && [ ${#digest} -eq 64 ] || { echo "appimagetool release has no digest" >&2; exit 1; }
curl -fsSL -o "$build/appimagetool" "$url"
echo "$digest  $build/appimagetool" | sha256sum -c --quiet -
chmod +x "$build/appimagetool"

out=$root/dist/clipplus-manager-$version-x86_64.AppImage
ARCH=x86_64 "$build/appimagetool" --appimage-extract-and-run --no-appstream "$appdir" "$out"
(cd "$root/dist" && sha256sum "$(basename "$out")" >"$(basename "$out").sha256")
echo "built $out"
