# ClipPlus Manager

A small, fast, native KDE desktop app. The user pastes links, the app downloads audio as mp3, fixes each file so a SanDisk Sansa Clip+ plays it, and copies the songs to the player's SD card when it is plugged in.

## Direction

Tradeoffs, in order:

1. The Sansa firmware rules are the source of truth. A song that does not show up or play on the player is a bug. The rules live in `core/src/rules.rs`.
2. Fast and light. Startup to first frame under about 300 ms, near-zero idle CPU, low memory. Everything is event driven: no polling loops, no timers that wake the app.
3. Native KDE. QML with Kirigami and QtQuick Controls, linked against the host Qt 6. No bundled Qt, no hard-coded colors or fonts.
4. Managed tools stay current on their own (ffmpeg, yt-dlp, deno), and so does the app. Never block the UI or a running download for an update, and never swap a binary while a process uses it.
5. Keep it simple. Small blocking crates on worker threads, no async runtime.

The CI container is `registry.fedoraproject.org/fedora:45`, pinned by digest. The AppImage links the host Qt, so the CI Fedora must never be newer than the Fedora the users run. Renovate must not move it to a new major release.

## Layout

- `core/`: Qt-free library (`clipplus-manager-core`). Rules, fixer, transfer, yt-dlp, tools, engine, UDisks2, notifications, single instance, self update.
- `src/`: the Qt app. `bridge.rs` is the cxx-qt QObject (list model and app state), `main.rs` starts Qt.
- `qml/`: the Kirigami UI, compiled into the binary.
- `packaging/appimage/`: AppImage build and tool fetch scripts.

The Qt build script links Qt into every target of its package, which is why the Qt-free code is a separate crate: its tests link without Qt.

## Commands

```
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
packaging/appimage/build.sh
```

## Rules

- Never touch a real SANSA card in tests. Use a vfat image labeled `SANSA` attached as a loop device.
- Public text (code, docs, commits, PRs) never names machines, IPs, home paths, real song names or accounts.
- Feature work goes through a PR with squash merge. Commits follow Conventional Commits, one sentence.
- Code comments say why, in a few words.

## Glossary

- **Sansa**: the SanDisk Sansa Clip+ player. Its firmware is picky about ID3 tags, cover art and FLAC formats.
- **Card**: the player's SD card, a vfat volume labeled `SANSA`. Songs go flat into `Tracks/`.
- **Outbox**: the folder of finished songs that wait for the card. It is the persistent queue for the copy step.
- **Fixer**: `plan()` picks the ffmpeg arguments that make a file playable, `verify()` rechecks the result.
- **Managed tools**: ffmpeg, ffprobe, yt-dlp and deno, kept in the data folder and updated from their upstream releases. The AppImage bundles the versions current at build time.
- **Self update**: the AppImage replaces itself from this repo's latest release, then asks for a restart.
