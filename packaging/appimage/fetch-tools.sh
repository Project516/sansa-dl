#!/usr/bin/env bash
# Downloads the newest ffmpeg, ffprobe, yt-dlp and deno into DIR and checks
# each against its published checksum. Writes DIR/versions.json.
# Usage: fetch-tools.sh DIR
set -euo pipefail

dir=${1:?usage: fetch-tools.sh DIR}
mkdir -p "$dir"
dir=$(cd "$dir" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

latest_tag() {
  curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/$1/releases/latest" | sed 's#.*/tag/##'
}

verify() { # FILE HASH
  echo "$2  $1" | sha256sum -c --quiet -
}

# yt-dlp
tag=$(latest_tag yt-dlp/yt-dlp)
base="https://github.com/yt-dlp/yt-dlp/releases/download/$tag"
curl -fsSL -o "$tmp/yt-dlp_linux" "$base/yt-dlp_linux"
curl -fsSL -o "$tmp/SHA2-256SUMS" "$base/SHA2-256SUMS"
hash=$(awk '$2 == "yt-dlp_linux" || $2 == "*yt-dlp_linux" {print $1}' "$tmp/SHA2-256SUMS")
(cd "$tmp" && verify yt-dlp_linux "$hash")
install -m755 "$tmp/yt-dlp_linux" "$dir/yt-dlp"
ytdlp_version=${tag}

# deno
tag=$(latest_tag denoland/deno)
base="https://github.com/denoland/deno/releases/download/$tag"
curl -fsSL -o "$tmp/deno.zip" "$base/deno-x86_64-unknown-linux-gnu.zip"
curl -fsSL -o "$tmp/deno.zip.sha256sum" "$base/deno-x86_64-unknown-linux-gnu.zip.sha256sum"
hash=$(awk '{print $1; exit}' "$tmp/deno.zip.sha256sum")
(cd "$tmp" && verify deno.zip "$hash")
unzip -p "$tmp/deno.zip" deno >"$dir/deno"
chmod 755 "$dir/deno"
deno_version=${tag#v}

# ffmpeg: the newest stable branch build
base="https://github.com/BtbN/FFmpeg-Builds/releases/download/latest"
curl -fsSL -o "$tmp/checksums.sha256" "$base/checksums.sha256"
pick=$(sed -nE 's/^([0-9a-f]{64}) +\*?(ffmpeg-n([0-9]+\.[0-9]+)-latest-linux64-gpl-\3\.tar\.xz)$/\3 \1 \2/p' "$tmp/checksums.sha256" | sort -V | tail -1)
[ -n "$pick" ] || { echo "no stable ffmpeg build listed" >&2; exit 1; }
read -r ffmpeg_version hash name <<<"$pick"
curl -fsSL -o "$tmp/$name" "$base/$name"
(cd "$tmp" && verify "$name" "$hash")
tar -xJf "$tmp/$name" -C "$dir" --strip-components=2 --wildcards '*/bin/ffmpeg' '*/bin/ffprobe'

for tool in "$dir/yt-dlp" "$dir/deno"; do "$tool" --version >/dev/null; done
for tool in "$dir/ffmpeg" "$dir/ffprobe"; do "$tool" -version >/dev/null; done

printf '{\n  "deno": "%s",\n  "ffmpeg": "%s",\n  "yt-dlp": "%s"\n}\n' \
  "$deno_version" "$ffmpeg_version" "$ytdlp_version" >"$dir/versions.json"
echo "tools in $dir: yt-dlp $ytdlp_version, deno $deno_version, ffmpeg $ffmpeg_version"
