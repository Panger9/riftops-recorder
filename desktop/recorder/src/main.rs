//! RiftOps recorder: records one League of Legends game with libobs.
//!
//! The desktop app starts one process per game:
//!
//! ```text
//! riftops-recorder --out <video.mp4> --log <obs.log> [--exe "League of Legends.exe"] [--fps 30]
//! ```
//!
//! It prints one JSON object per line on stdout (`waiting`, `recording`,
//! `stopped`, `error`) and stops when stdin sends `stop` or closes. The
//! recording starts only once the capture delivers frames, so the app can
//! take the `recording` event as the first video frame.
//!
//! Capture: OBS game capture on the game window (works in fullscreen, with
//! the game's own audio). If the game cannot be hooked, for example because
//! another recorder already holds the hook, it falls back to window capture,
//! which needs windowed or borderless mode.

use std::{
    io::{BufRead, Write},
    path::PathBuf,
    sync::mpsc::{self, Receiver, RecvTimeoutError},
    thread,
    time::{Duration, Instant},
};

use libobs_simple::sources::windows::{
    GameCaptureSourceBuilder, ObsGameCaptureMode, ObsHookableSourceTrait, ObsWindowCaptureMethod,
    WindowCaptureSourceBuilder,
};
use libobs_window_helper::{get_all_windows, is_window_in_use_by_other_instance, WindowInfo, WindowSearchMode};
use libobs_wrapper::{
    context::ObsContext,
    data::{output::ObsOutputTrait, video::ObsVideoInfoBuilder, ObsDataSetters},
    encoders::{ObsAudioEncoderType, ObsContextEncoders, ObsVideoEncoderType},
    logger::FileLogger,
    scenes::{ObsSceneRef, SceneItemExtSceneTrait},
    sources::ObsSourceBuilder,
    utils::{AudioEncoderInfo, OutputInfo, StartupInfo, VideoEncoderInfo},
};
use serde_json::{json, Value};
use windows::Win32::{
    Foundation::{HWND, RECT},
    UI::{
        HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2},
        WindowsAndMessaging::GetClientRect,
    },
};

/// How long the game window may take to appear after the app starts us.
const WINDOW_WAIT: Duration = Duration::from_secs(120);
/// How long game capture may take to hook before window capture takes over.
const HOOK_WAIT: Duration = Duration::from_secs(20);
/// Recordings are at most this tall; larger windows are scaled down.
const MAX_HEIGHT: u32 = 1080;
const AUDIO_KBPS: i64 = 160;

struct Args {
    out: PathBuf,
    log: PathBuf,
    exe: String,
    fps: u32,
}

fn parse_args() -> Result<Args, String> {
    let mut out = None;
    let mut log = None;
    let mut exe = "League of Legends.exe".to_string();
    let mut fps = 30;
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let value = args.next().ok_or(format!("{flag} needs a value"))?;
        match flag.as_str() {
            "--out" => out = Some(PathBuf::from(value)),
            "--log" => log = Some(PathBuf::from(value)),
            "--exe" => exe = value,
            "--fps" => fps = value.parse().map_err(|_| "--fps needs a number")?,
            _ => return Err(format!("unknown flag {flag}")),
        }
    }
    Ok(Args {
        out: out.ok_or("--out is missing")?,
        log: log.ok_or("--log is missing")?,
        exe,
        fps: fps.clamp(15, 60),
    })
}

fn emit(value: Value) {
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(stdout, "{value}");
    let _ = stdout.flush();
}

fn fail(message: impl std::fmt::Display) -> ! {
    emit(json!({ "event": "error", "message": message.to_string() }));
    std::process::exit(1);
}

/// Lines from the app; a closed stdin counts as `stop`.
fn read_commands() -> Receiver<String> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if tx.send(line.trim().to_string()).is_err() {
                return;
            }
        }
        let _ = tx.send("stop".to_string());
    });
    rx
}

/// Waits up to `timeout` for `stop`. Returns true when it came.
fn stop_requested(commands: &Receiver<String>, timeout: Duration) -> bool {
    match commands.recv_timeout(timeout) {
        Ok(line) => line == "stop",
        Err(RecvTimeoutError::Timeout) => false,
        Err(RecvTimeoutError::Disconnected) => true,
    }
}

fn find_window(exe: &str) -> Option<WindowInfo> {
    let exe = exe.to_lowercase();
    get_all_windows(WindowSearchMode::ExcludeMinimized)
        .ok()?
        .into_iter()
        .find(|window| {
            let path = window.full_exe.to_lowercase();
            (path == exe || path.ends_with(&format!("\\{exe}"))) && client_size(window.handle).is_some()
        })
}

fn client_size(handle: HWND) -> Option<(u32, u32)> {
    let mut rect = RECT::default();
    // Safety: GetClientRect only writes into the RECT we own.
    unsafe { GetClientRect(handle, &mut rect) }.ok()?;
    let (width, height) = ((rect.right - rect.left) as u32, (rect.bottom - rect.top) as u32);
    (width >= 320 && height >= 240).then_some((width, height))
}

/// Output size: at most MAX_HEIGHT tall, same aspect. libobs rounds the
/// width down to a multiple of 4 and the height to an even number.
fn output_size(width: u32, height: u32) -> (u32, u32) {
    let (width, height) = if height <= MAX_HEIGHT { (width, height) } else { (width * MAX_HEIGHT / height, MAX_HEIGHT) };
    (width / 4 * 4, height / 2 * 2)
}

/// Peak bitrate that keeps fights readable: about 6 Mbit/s at 1080p30.
fn peak_kbps(width: u32, height: u32, fps: u32) -> i64 {
    (width as i64 * height as i64 * fps as i64 / 10_000).clamp(2_500, 8_000)
}

/// Average bitrate of the variable rate: half the peak. Lane phases and
/// the map change little and take far less, fights take up to the peak.
fn average_kbps(width: u32, height: u32, fps: u32) -> i64 {
    peak_kbps(width, height, fps) / 2
}

fn pick_encoder(context: &ObsContext) -> ObsVideoEncoderType {
    let available = context
        .available_video_encoders()
        .map(|list| list.into_iter().map(|encoder| encoder.get_encoder_id().clone()).collect::<Vec<_>>())
        .unwrap_or_default();
    [
        ObsVideoEncoderType::OBS_NVENC_H264_TEX,
        ObsVideoEncoderType::H264_TEXTURE_AMF,
        ObsVideoEncoderType::OBS_QSV11_V2,
    ]
    .into_iter()
    .find(|candidate| available.contains(candidate))
    .unwrap_or(ObsVideoEncoderType::OBS_X264)
}

enum Capture {
    Game,
    Window,
}

/// Adds game capture for the window and waits until it delivers frames.
/// Falls back to window capture when the hook does not come.
fn add_capture(scene: &mut ObsSceneRef, context: &ObsContext, window: &WindowInfo, commands: &Receiver<String>) -> Capture {
    let taken = is_window_in_use_by_other_instance(window.pid).unwrap_or(false);
    if !taken {
        let builder = context
            .source_builder::<GameCaptureSourceBuilder, _>("game")
            .unwrap_or_else(|error| fail(format!("game capture: {error:?}")))
            .set_capture_mode(ObsGameCaptureMode::CaptureSpecificWindow)
            .set_window_raw(window.obs_id.as_str())
            .set_capture_cursor(true);
        let builder = builder.set_capture_audio(true).unwrap_or_else(|error| fail(format!("game audio: {error:?}")));
        let source = builder.build().unwrap_or_else(|error| fail(format!("game capture: {error:?}")));
        let mut hooked = source
            .source_specific_signals()
            .on_hooked()
            .unwrap_or_else(|error| fail(format!("game capture signal: {error:?}")));
        let item = scene.add_source(source).unwrap_or_else(|error| fail(format!("game capture: {error:?}")));
        let started = Instant::now();
        while started.elapsed() < HOOK_WAIT {
            if hooked.try_recv().is_ok() {
                return Capture::Game;
            }
            if stop_requested(commands, Duration::from_millis(200)) {
                emit(json!({ "event": "stopped", "sizeBytes": 0 }));
                std::process::exit(0);
            }
        }
        let _ = scene.remove_scene_item(item);
    }

    let builder = context
        .source_builder::<WindowCaptureSourceBuilder, _>("window")
        .unwrap_or_else(|error| fail(format!("window capture: {error:?}")))
        .set_window_raw(window.obs_id.as_str())
        .set_capture_method(ObsWindowCaptureMethod::MethodWgc)
        .set_client_area(true)
        .set_cursor(true);
    let builder = builder.set_capture_audio(true).unwrap_or_else(|error| fail(format!("window audio: {error:?}")));
    builder
        .add_to_scene(scene)
        .unwrap_or_else(|error| fail(format!("window capture: {error:?}")));
    Capture::Window
}

fn main() {
    let args = parse_args().unwrap_or_else(|error| fail(error));
    // Physical pixels, so the canvas matches the game's real resolution.
    // Safety: called once before any window or OBS work.
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    let commands = read_commands();

    emit(json!({ "event": "waiting" }));
    let started = Instant::now();
    let window = loop {
        if let Some(window) = find_window(&args.exe) {
            break window;
        }
        if started.elapsed() > WINDOW_WAIT {
            fail("game window not found");
        }
        if stop_requested(&commands, Duration::from_millis(500)) {
            emit(json!({ "event": "stopped", "sizeBytes": 0 }));
            return;
        }
    };
    let (width, height) = client_size(window.handle).unwrap_or_else(|| fail("game window has no size"));
    let (out_width, out_height) = output_size(width, height);

    let logger = FileLogger::from_file(&args.log).unwrap_or_else(|error| fail(format!("log: {error:?}")));
    let video = ObsVideoInfoBuilder::new()
        .fps_num(args.fps)
        .fps_den(1)
        .base_width(width)
        .base_height(height)
        .output_width(out_width)
        .output_height(out_height)
        .build();
    let mut context = StartupInfo::new()
        .set_logger(Box::new(logger))
        .set_video_info(video)
        .start()
        .unwrap_or_else(|error| fail(format!("obs start: {error:?}")));
    let mut scene = context
        .scene("main", Some(0))
        .unwrap_or_else(|error| fail(format!("scene: {error:?}")));
    let capture = add_capture(&mut scene, &context, &window, &commands);

    let mut output_settings = context.data().unwrap_or_else(|error| fail(format!("{error:?}")));
    let _ = output_settings.set_string("path", args.out.to_string_lossy().as_ref());
    let mut output = context
        .output(OutputInfo::new("mp4_output", "recording", Some(output_settings), None))
        .unwrap_or_else(|error| fail(format!("output: {error:?}")));

    let encoder = pick_encoder(&context);
    let mut video_settings = context.data().unwrap_or_else(|error| fail(format!("{error:?}")));
    let average = average_kbps(out_width, out_height, args.fps);
    let peak = peak_kbps(out_width, out_height, args.fps);
    let _ = video_settings.set_string("rate_control", "VBR");
    let _ = video_settings.set_int("bitrate", average);
    let _ = video_settings.set_int("max_bitrate", peak);
    // A keyframe every two seconds keeps seeking and perspective switches quick at a smaller file.
    let _ = video_settings.set_int("keyint_sec", 2);
    let _ = video_settings.set_string("profile", "high");
    match encoder {
        ObsVideoEncoderType::OBS_NVENC_H264_TEX => {
            let _ = video_settings.set_string("preset2", "p5");
            let _ = video_settings.set_string("tune", "hq");
        }
        ObsVideoEncoderType::H264_TEXTURE_AMF => {
            // AMF has no peak setting: its VBR peaks at 1.5 times the target.
            let _ = video_settings.set_string("rate_control", "vbr");
            let _ = video_settings.set_int("bitrate", peak * 2 / 3);
            let _ = video_settings.set_string("preset", "quality");
        }
        ObsVideoEncoderType::OBS_X264 => {
            // x264's VBR is a quality target with the bitrate as its cap.
            let _ = video_settings.set_int("crf", 23);
            let _ = video_settings.set_int("bitrate", peak);
            let _ = video_settings.set_string("preset", "veryfast");
        }
        _ => {}
    }
    let encoder_name = format!("{encoder:?}");
    output
        .create_and_set_video_encoder(VideoEncoderInfo::new(encoder, "video", Some(video_settings), None))
        .unwrap_or_else(|error| fail(format!("video encoder: {error:?}")));
    let mut audio_settings = context.data().unwrap_or_else(|error| fail(format!("{error:?}")));
    let _ = audio_settings.set_string("rate_control", "CBR");
    let _ = audio_settings.set_int("bitrate", AUDIO_KBPS);
    output
        .create_and_set_audio_encoder(
            AudioEncoderInfo::new(ObsAudioEncoderType::FFMPEG_AAC, "audio", Some(audio_settings), None),
            0,
        )
        .unwrap_or_else(|error| fail(format!("audio encoder: {error:?}")));

    output.start().unwrap_or_else(|error| fail(format!("recording start: {error:?}")));
    emit(json!({
        "event": "recording",
        "width": out_width,
        "height": out_height,
        "encoder": encoder_name,
        "capture": match capture { Capture::Game => "game", Capture::Window => "window" },
    }));

    while !stop_requested(&commands, Duration::from_millis(500)) {}
    output.stop().unwrap_or_else(|error| fail(format!("recording stop: {error:?}")));
    let size = std::fs::metadata(&args.out).map(|meta| meta.len()).unwrap_or(0);
    emit(json!({ "event": "stopped", "sizeBytes": size }));
    drop(output);
    drop(scene);
    drop(context);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scales_down_to_1080p() {
        assert_eq!(output_size(1920, 1080), (1920, 1080));
        assert_eq!(output_size(2560, 1440), (1920, 1080));
        assert_eq!(output_size(3440, 1440), (2580, 1080));
        assert_eq!(output_size(1578, 729), (1576, 728));
    }

    #[test]
    fn bitrate_follows_the_picture() {
        assert_eq!(peak_kbps(1920, 1080, 30), 6_220);
        assert_eq!(peak_kbps(1280, 720, 30), 2_764);
        assert_eq!(peak_kbps(800, 600, 30), 2_500);
        assert_eq!(peak_kbps(1920, 1080, 60), 8_000);
        assert_eq!(average_kbps(1920, 1080, 30), 3_110);
        assert_eq!(average_kbps(800, 600, 30), 1_250);
    }
}
