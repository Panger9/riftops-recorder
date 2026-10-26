# RiftOps recorder

Read-only mirror of the recorder of the [RiftOps](https://riftops.fp-labs.net) desktop app, version 0.2.6. Nobody works in this repository; it is rewritten from the RiftOps source for every desktop release, and each release has a tag (`desktop-v<version>`).

The recorder records one League of Legends game (OBS game capture with the game's audio) and runs as its own process next to the desktop app. It links libobs and is therefore free software under the GNU General Public License v3 or later, see [LICENSE](LICENSE).

## Build

Windows x64 with [Rust](https://rustup.rs) and Node.js 22:

```bash
node scripts/build-desktop-recorder.mjs
```

This builds `riftops-recorder.exe` and copies the needed part of the official OBS Studio 32.0.4 build (version and checksum are pinned in the script) next to it into `desktop/recorder/dist`. More in [desktop/recorder/README.md](desktop/recorder/README.md), where `npm run desktop:recorder` stands for the command above.

## OBS Studio

The OBS files in the installer (libobs, capture and encoder plugins, FFmpeg and x264 libraries) are unmodified files of the official OBS Studio 32.0.4 release, licensed under the GPL v2 or later. Their exact source code: https://github.com/obsproject/obs-studio/tree/32.0.4
