# RiftOps recorder

Records one League of Legends game for the RiftOps desktop app: OBS game capture of the game window (fullscreen included) with the game's audio, hardware H.264 at variable bitrate, one keyframe every two seconds, MP4.

It is a separate program under the GPL v3 or later because it links libobs (GPL v2 or later). The desktop app only starts it and talks to it over stdin and stdout; the protocol is described at the top of [src/main.rs](src/main.rs).

## Build

```bash
npm run desktop:recorder
```

builds `riftops-recorder.exe` and copies the needed part of the official OBS Studio build (version and checksum pinned in `scripts/build-desktop-recorder.mjs`) into `dist/`. Rust bindings: [libobs-rs](https://github.com/libobs-rs/libobs-rs).

## Try it

```bash
desktop/recorder/dist/riftops-recorder.exe --out test.mp4 --log test.log --exe notepad.exe
```

Type `stop` to finish the file. Windows without a game hook fall back to window capture after 20 seconds.
