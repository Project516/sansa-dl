# Sansa DL

Sansa DL is a small KDE app that turns links into songs for a SanDisk Sansa Clip+. Paste YouTube links, or any link yt-dlp supports. The app downloads the audio as mp3, makes each file playable on the Sansa, and copies the songs to the player's SD card when you plug it in.

## What it does

- Downloads audio only, as mp3, with SponsorBlock segments removed (you can turn that off).
- Expands playlists and channels into one row per song. Up to three songs download at once.
- Fixes each file for the Sansa firmware: no embedded cover art, a small ID3 tag, and the audio format the player accepts. mp3 files are never re-encoded just to change the sample rate.
- Keeps finished songs in an outbox until the card is plugged in. The queue survives a restart.
- Copies the songs into `Tracks/` on the card, checks each copy byte by byte, then deletes the local file. It never overwrites or deletes a file on the card that it did not just write.
- Shows "Safe to unplug the Sansa" after you press Eject.
- Keeps yt-dlp, ffmpeg and deno current on its own, and updates itself.

## Install

1. Download `sansa-dl-<version>-x86_64.AppImage` from the [Releases](https://github.com/Project516/sansa-dl/releases) page.
2. Make it executable: `chmod +x sansa-dl-*.AppImage`.
3. Run it once. It adds a launcher entry, so "Sansa DL" shows up in the application menu from then on.

The app updates itself once a day. After an update it asks you to restart.

## Requirements

- Linux x86_64 with a Plasma 6 desktop. The AppImage links the system Qt and Kirigami, so it needs:
  - Qt 6.11 or newer (the release is built on Fedora 45)
  - Kirigami 6 and `qqc2-desktop-style` from KDE Frameworks 6
- UDisks2 on the system bus (any desktop that mounts removable media has it).
- Internet access for downloads and tool updates.

## Use

Paste one or more links into the field and press Enter or Add. You can also drop links on the window. The field clears at once, so you can paste the next link right away.

```
./sansa-dl-<version>-x86_64.AppImage <link>...   add links to the running window, or start the app with them
./sansa-dl-<version>-x86_64.AppImage --eject     eject the card, as the Eject button does
```

The launcher entry runs the same AppImage.

When the Sansa's card is plugged in (a FAT32 volume labeled `SANSA`), the app mounts it and copies the waiting songs. A desktop notification tells you how many were copied and offers Eject.

Songs are stored in `~/.local/share/sansa-dl/`: `downloads/` while a song downloads, `outbox/` while it waits for the card. The settings file is `~/.config/sansa-dl/config.json`.

## Remove the launcher entry

Delete these two files:

```
~/.local/share/applications/dev.project516.sansa-dl.desktop
~/.local/share/icons/hicolor/scalable/apps/dev.project516.sansa-dl.svg
```

To remove everything the app stored, also delete `~/.local/share/sansa-dl` and `~/.config/sansa-dl`.

## Build from source

You need Rust (edition 2024), a C++ compiler, and the Qt 6 development packages (`qtbase` and `qtdeclarative`). Kirigami must be installed to run the app.

```
cargo build --release
cargo test --workspace
packaging/appimage/build.sh     # builds the AppImage into dist/
```

On Fedora: `dnf install gcc-c++ qt6-qtbase-devel qt6-qtdeclarative-devel kf6-kirigami qqc2-desktop-style`.

The tests that make audio fixtures need an `ffmpeg` with the LAME encoder on `PATH`. They skip when it is missing. `packaging/appimage/fetch-tools.sh <dir>` downloads one.

## Browser version

The browser version is at https://project516.dev/sansa-dl/. GitHub Pages deploys
it from `web/` on every push to `master` that changes it.

It downloads one song as an MP3 for manual transfer to the
Sansa's `Tracks/` folder. It needs a browser with JavaScript Promise Integration
(JSPI) and a compatible [yt-dlp-wasm proxy](https://github.com/Project516/yt-dlp-wasm/tree/master/packages/cors-proxy)
that allows the page's origin. Enter your personal proxy URL and access key in
Personal proxy settings. The settings stay in browser storage, and the proxy
handles download requests. Some proxies may be blocked by YouTube.

To run it locally, install Node.js 25 or newer and pnpm, then run:

```
cd web
pnpm install
pnpm dev
```

Run `pnpm build` to create the static site in `web/dist/`. Conversion runs in
the browser and may take time and memory for long recordings.

## License

AGPL-3.0-only. See [LICENSE](LICENSE).
