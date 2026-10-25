// Builds desktop/recorder/dist: riftops-recorder.exe plus the parts of the
// official, signed OBS Studio build it needs (capture, encoders, game audio,
// MP4 output). The desktop installer ships this folder as a resource.
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { copyFileSync, cpSync, createReadStream, createWriteStream, existsSync, mkdirSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import path from "node:path";
import { Readable } from "node:stream";
import { pipeline } from "node:stream/promises";

// Must match the libobs crate the recorder links against (desktop/recorder/Cargo.lock).
const obsVersion = "32.0.4";
const obsZipSha256 = "fd367a7f156319426f1a600720c1288ad0011da168dff41e0f94dbb18cc1dc0f";
const plugins = ["win-capture", "win-wasapi", "obs-ffmpeg", "obs-nvenc", "obs-qsv11", "obs-x264", "obs-outputs"];
// libobs, its Direct3D and capture backends, FFmpeg and the encoder probes. No UI, browser or scripting.
const binaries = new Set([
  "obs.dll",
  "libobs-d3d11.dll",
  "libobs-winrt.dll",
  "w32-pthreads.dll",
  "zlib.dll",
  "libcurl.dll",
  "srt.dll",
  "librist.dll",
  "obs-amf-test.exe",
  "obs-nvenc-test.exe",
  "obs-qsv-test.exe",
  "obs-ffmpeg-mux.exe",
]);
const versionedBinary = /^(avcodec|avdevice|avfilter|avformat|avutil|swresample|swscale|libx264)-\d+\.dll$/;

const root = path.resolve(import.meta.dirname, "..");
const recorder = path.join(root, "desktop/recorder");
const cache = path.join(recorder, ".obs-cache");
const dist = path.join(recorder, "dist");
const zipName = `OBS-Studio-${obsVersion}-Windows-x64.zip`;
const zip = path.join(cache, zipName);
const extracted = path.join(cache, obsVersion);

// Ships next to the recorder in the installer; the GPL requires the license and a way to the source.
function notice() {
  return [
    "RiftOps recorder",
    "",
    "riftops-recorder.exe is free software under the GNU General Public License v3 or later (see LICENSE).",
    "Its source code, with a tag for every desktop version: https://github.com/Panger9/riftops-recorder",
    "",
    `The other files in this folder are unmodified files of the official OBS Studio ${obsVersion} release (https://obsproject.com):`,
    "- OBS Studio: libobs and the capture, audio and encoder plugins (GNU GPL v2 or later)",
    "- FFmpeg libraries (avcodec, avformat and others) and x264 (libx264), as built by the OBS project (GNU GPL)",
    "",
    `Source code of OBS Studio ${obsVersion}: https://github.com/obsproject/obs-studio/tree/${obsVersion}`,
    "Source code of its FFmpeg and x264 builds: https://github.com/obsproject/obs-deps",
    "",
  ].join("\n");
}

async function sha256(file) {
  const hash = createHash("sha256");
  await pipeline(createReadStream(file), hash);
  return hash.digest("hex");
}

mkdirSync(cache, { recursive: true });
if (!existsSync(zip) || (await sha256(zip)) !== obsZipSha256) {
  console.log(`Downloading ${zipName}`);
  const response = await fetch(`https://github.com/obsproject/obs-studio/releases/download/${obsVersion}/${zipName}`);
  if (!response.ok) throw new Error(`OBS download failed: ${response.status}`);
  await pipeline(Readable.fromWeb(response.body), createWriteStream(zip));
  if ((await sha256(zip)) !== obsZipSha256) throw new Error("OBS download does not match the pinned checksum");
}
if (!existsSync(path.join(extracted, "bin/64bit/obs.dll"))) {
  rmSync(extracted, { recursive: true, force: true });
  mkdirSync(extracted, { recursive: true });
  // Windows' own tar (bsdtar) reads zip files; Git's GNU tar does not.
  const tar = path.join(process.env.SystemRoot ?? "C:\\Windows", "System32", "tar.exe");
  execFileSync(tar, ["-xf", zip, "-C", extracted, "bin/64bit", "obs-plugins/64bit", "data/libobs", "data/obs-plugins"], { stdio: "inherit" });
}

// rustup installs cargo under CARGO_HOME, locally and on the CI runner.
const cargo = path.join(process.env.CARGO_HOME ?? path.join(homedir(), ".cargo"), "bin", "cargo.exe");
execFileSync(cargo, ["build", "--release", "--locked", "--manifest-path", path.join(recorder, "Cargo.toml")], { cwd: root, stdio: "inherit" });

rmSync(dist, { recursive: true, force: true });
mkdirSync(path.join(dist, "obs-plugins/64bit"), { recursive: true });
copyFileSync(path.join(recorder, "target/release/riftops-recorder.exe"), path.join(dist, "riftops-recorder.exe"));
for (const file of readdirSync(path.join(extracted, "bin/64bit"))) {
  if (binaries.has(file) || versionedBinary.test(file)) copyFileSync(path.join(extracted, "bin/64bit", file), path.join(dist, file));
}
cpSync(path.join(extracted, "data/libobs"), path.join(dist, "data/libobs"), { recursive: true });
for (const plugin of plugins) {
  copyFileSync(path.join(extracted, "obs-plugins/64bit", `${plugin}.dll`), path.join(dist, "obs-plugins/64bit", `${plugin}.dll`));
  const data = path.join(extracted, "data/obs-plugins", plugin);
  if (!existsSync(data)) continue;
  cpSync(data, path.join(dist, "data/obs-plugins", plugin), {
    recursive: true,
    filter: (source) => !source.endsWith(".pdb") && !(source.includes(`${path.sep}locale${path.sep}`) && !source.endsWith("en-US.ini")),
  });
}
cpSync(path.join(recorder, "LICENSE"), path.join(dist, "LICENSE"));
writeFileSync(path.join(dist, "NOTICE.txt"), notice());
console.log("Recorder ready: desktop/recorder/dist");
