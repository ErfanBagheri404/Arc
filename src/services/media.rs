//! Media session provider: SMTC transport state + album art, off the UI thread.
//!
//! Windows' `GlobalSystemMediaTransportControlsSessionManager` is WinRT and its
//! calls are async. The UI thread must never block on them, so this module owns a
//! dedicated worker thread with its own COM apartment (MTA) and publishes an
//! immutable `MediaState` snapshot into a shared cell. The UI reads the last
//! snapshot; it never calls WinRT directly.
//!
//! Contract:
//! - [`Media::start`] once, after COM exists on the *calling* thread.
//! - [`Media::snapshot`] any number of times, from the UI thread, never blocking.
//! - Transport actions are fire-and-forget requests onto the worker queue.

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::core::Interface;
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSessionManager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus,
};
use windows::Storage::Streams::{DataReader, IInputStream, InputStreamOptions};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

/// What the UI needs to draw the Media tab. Plain data: no COM handles escape.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MediaState {
    pub title: String,
    pub artist: String,
    pub album: String,
    /// Source app id, e.g. `Spotify.exe` — shown so the user knows what is playing.
    pub source: String,
    pub playing: bool,
    pub position: Duration,
    pub duration: Option<Duration>,
    /// Handle into `core::imagedb` for the current cover, if one decoded.
    pub art: Option<u64>,
    /// Accent extracted from the art, when art exists.
    pub accent: Option<crate::core::geom::Rgba>,
}

impl MediaState {
    /// Progress in `0.0..=1.0`, or `None` for live streams with no duration.
    pub fn progress(&self) -> Option<f32> {
        let total = self.duration?.as_secs_f32();
        if total <= 0.0 {
            return None;
        }
        Some((self.position.as_secs_f32() / total).clamp(0.0, 1.0))
    }

    /// True when there is a session worth showing.
    pub fn has_session(&self) -> bool {
        !self.title.is_empty() || !self.source.is_empty()
    }
}

/// Fire-and-forget transport commands, applied on the worker thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    PlayPause,
    Next,
    Previous,
    /// Seek to an absolute position, in seconds.
    Seek(u64),
}

/// Handle to the running sampler. Dropping it stops the worker.
pub struct Media {
    state: Arc<Mutex<MediaState>>,
    commands: Sender<Command>,
    stop: Arc<Mutex<bool>>,
}

impl Media {
    /// Spawn the worker. Cheap and non-blocking: the first snapshot is `Default`
    /// until the worker's first successful poll.
    pub fn start() -> Self {
        let state = Arc::new(Mutex::new(MediaState::default()));
        let (commands, queue) = mpsc::channel();
        let stop = Arc::new(Mutex::new(false));

        let worker_state = Arc::clone(&state);
        let worker_stop = Arc::clone(&stop);
        std::thread::Builder::new()
            .name("arc-media".into())
            .spawn(move || {
                // The worker owns its apartment: WinRT activation from an MTA
                // thread is what lets `GetCurrentSession` be called at all.
                let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
                run(worker_state, queue, worker_stop);
            })
            .ok();

        Self {
            state,
            commands,
            stop,
        }
    }

    /// Last published snapshot. Never blocks on the worker.
    pub fn snapshot(&self) -> MediaState {
        self.state
            .lock()
            .map(|s| s.clone())
            .unwrap_or_default()
    }

    /// Queue a transport action. Silently dropped if the worker has exited.
    pub fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }
}

impl Drop for Media {
    fn drop(&mut self) {
        if let Ok(mut stop) = self.stop.lock() {
            *stop = true;
        }
    }
}

/// Poll cadence. 1 Hz is enough for a progress bar that ticks per second; the
/// position is interpolated by the UI between ticks, so this stays cheap.
const POLL_INTERVAL: Duration = Duration::from_secs(1);

fn run(state: Arc<Mutex<MediaState>>, queue: Receiver<Command>, stop: Arc<Mutex<bool>>) {
    let mut manager: Option<GlobalSystemMediaTransportControlsSessionManager> = None;
    let mut last_art_key: Option<String> = None;
    let mut next_poll = Instant::now();

    loop {
        if stop.lock().map(|s| *s).unwrap_or(true) {
            return;
        }

        // Drain commands first so a click feels immediate.
        while let Ok(command) = queue.try_recv() {
            if let Some(session) = current_session(&mut manager) {
                let _ = apply(&session, command);
            }
        }

        if Instant::now() >= next_poll {
            next_poll = Instant::now() + POLL_INTERVAL;
            if let Some(session) = current_session(&mut manager) {
                let mut snapshot = read(&session);
                if snapshot.art.is_none() {
                    // Cover fetch is the expensive part; only when the track key
                    // changed, and only for the current session.
                    let key = format!("{}\u{1}{}\u{1}{}", snapshot.source, snapshot.title, snapshot.album);
                    if last_art_key.as_deref() != Some(key.as_str()) {
                        last_art_key = Some(key);
                        if let Some((handle, accent)) = fetch_art(&session) {
                            snapshot.art = Some(handle.0);
                            snapshot.accent = Some(accent);
                        }
                    }
                }
                if let Ok(mut guard) = state.lock() {
                    *guard = snapshot;
                }
            } else if let Ok(mut guard) = state.lock() {
                // No session: clear, so a closed Spotify stops showing its last
                // track forever.
                *guard = MediaState::default();
                last_art_key = None;
            }
        }

        // Sleep, but stay responsive to a stop request.
        std::thread::sleep(POLL_INTERVAL.min(Duration::from_millis(250)));
    }
}

fn current_session(
    manager: &mut Option<GlobalSystemMediaTransportControlsSessionManager>,
) -> Option<windows::Media::Control::GlobalSystemMediaTransportControlsSession> {
    if manager.is_none() {
        *manager = GlobalSystemMediaTransportControlsSessionManager::RequestAsync()
            .ok()?
            .join()
            .ok();
    }
    manager.as_ref()?.GetCurrentSession().ok()
}

fn read(
    session: &windows::Media::Control::GlobalSystemMediaTransportControlsSession,
) -> MediaState {
    let mut state = MediaState::default();

    if let Ok(source) = session.SourceAppUserModelId() {
        state.source = source.to_string();
    }

    if let Ok(properties) = session.TryGetMediaPropertiesAsync().and_then(|op| op.join()) {
        state.title = properties.Title().map(|s| s.to_string()).unwrap_or_default();
        state.artist = properties.Artist().map(|s| s.to_string()).unwrap_or_default();
        state.album = properties.AlbumTitle().map(|s| s.to_string()).unwrap_or_default();
    }

    if let Ok(timeline) = session.GetTimelineProperties() {
        let position = timeline.Position().map(duration_of).unwrap_or_default();
        let end = timeline.EndTime().map(duration_of).unwrap_or_default();
        state.position = position;
        if end > Duration::ZERO {
            state.duration = Some(end);
        }
    }

    if let Ok(info) = session.GetPlaybackInfo() {
        if let Ok(status) = info.PlaybackStatus() {
            state.playing =
                status == GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing;
        }
    }

    state
}

fn duration_of(span: windows::Foundation::TimeSpan) -> Duration {
    // WinRT TimeSpan is 100 ns ticks.
    let ticks = span.Duration.max(0) as u64;
    Duration::from_nanos(ticks.saturating_mul(100))
}

fn apply(
    session: &windows::Media::Control::GlobalSystemMediaTransportControlsSession,
    command: Command,
) -> windows::core::Result<()> {
    match command {
        Command::PlayPause => {
            let playing = session
                .GetPlaybackInfo()
                .and_then(|i| i.PlaybackStatus())
                .map(|s| s == GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing)
                .unwrap_or(false);
            if playing {
                session.TryPauseAsync()?.join()?;
            } else {
                session.TryPlayAsync()?.join()?;
            }
            Ok(())
        }
        Command::Next => session.TrySkipNextAsync()?.join().map(|_| ()),
        Command::Previous => session.TrySkipPreviousAsync()?.join().map(|_| ()),
        // Clicking the bar seeks absolutely, so no current-position readback is
        // needed and the target can't drift from what the user clicked.
        Command::Seek(secs) => session
            .TryChangePlaybackPositionAsync((secs as i64) * 10_000_000)?
            .join()
            .map(|_| ()),
    }
}

/// Decode the cover into `core::imagedb` and extract its accent. Returns the
/// handle plus accent, or `None` when the session has no thumbnail.
fn fetch_art(
    session: &windows::Media::Control::GlobalSystemMediaTransportControlsSession,
) -> Option<(crate::core::scene::ImageHandle, crate::core::geom::Rgba)> {
    let reference = session
        .TryGetMediaPropertiesAsync()
        .and_then(|op| op.join())
        .ok()?
        .Thumbnail()
        .ok()?;
    let stream = reference.OpenReadAsync().ok()?.join().ok()?;
    let bytes = read_stream(&stream)?;
    if bytes.is_empty() {
        return None;
    }
    let handle = crate::core::imagedb::intern(&bytes);
    // Decode once for the accent. `extract_accent` wants raw RGBA, so this is a
    // separate WIC pass on the worker; the renderer decodes again for display,
    // which keeps the two caches independent and the UI thread free.
    let accent = decode_rgba(&bytes)
        .map(|(pixels, w, h)| crate::core::color::extract_accent(&pixels, w, h))
        .unwrap_or_else(|| crate::core::geom::Rgba::rgb(0.8, 0.2, 0.3));
    Some((handle, accent))
}

fn read_stream(stream: &windows::Storage::Streams::IRandomAccessStreamWithContentType) -> Option<Vec<u8>> {
    let size = stream.Size().ok()?;
    if size == 0 || size > 16 * 1024 * 1024 {
        return None;
    }
    let size = size as u32;
    let input = stream.clone().cast::<IInputStream>().ok()?;
    let reader = DataReader::CreateDataReader(&input).ok()?;
    reader.SetInputStreamOptions(InputStreamOptions::ReadAhead).ok()?;
    reader.LoadAsync(size).ok()?.join().ok()?;
    let mut bytes = vec![0u8; size as usize];
    reader.ReadBytes(&mut bytes).ok()?;
    Some(bytes)
}

/// Minimal RGBA decode for accent extraction, via WIC. Mirrors the renderer's
/// decode but yields CPU pixels instead of a D2D bitmap.
fn decode_rgba(bytes: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    use windows::Win32::Graphics::Imaging::{
        CLSID_WICImagingFactory, GUID_WICPixelFormat32bppRGBA, IWICImagingFactory,
        WICBitmapDitherTypeNone, WICBitmapPaletteTypeCustom, WICDecodeMetadataCacheOnDemand,
    };
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
    use windows::Win32::UI::Shell::SHCreateMemStream;

    unsafe {
        let factory: IWICImagingFactory = CoCreateInstance(
            &CLSID_WICImagingFactory,
            None,
            CLSCTX_INPROC_SERVER,
        )
        .ok()?;
        let stream = SHCreateMemStream(Some(bytes))?;
        let decoder = factory
            .CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand)
            .ok()?;
        let frame = decoder.GetFrame(0).ok()?;
        let converter = factory.CreateFormatConverter().ok()?;
        converter
            .Initialize(
                &frame,
                &GUID_WICPixelFormat32bppRGBA,
                WICBitmapDitherTypeNone,
                None::<&windows::Win32::Graphics::Imaging::IWICPalette>,
                0.0,
                WICBitmapPaletteTypeCustom,
            )
            .ok()?;
        let (mut w, mut h) = (0u32, 0u32);
        converter.GetSize(&mut w, &mut h).ok()?;
        if w == 0 || h == 0 {
            return None;
        }
        // Cap the pixels we copy: accent extraction downsamples anyway, so a
        // full-resolution copy of a 4K cover is wasted bandwidth.
        let (sw, sh) = (w.min(128), h.min(128));
        let stride = sw * 4;
        let mut pixels = vec![0u8; (stride * sh) as usize];
        let rect = windows::Win32::Graphics::Imaging::WICRect {
            X: 0,
            Y: 0,
            Width: sw as i32,
            Height: sh as i32,
        };
        converter.CopyPixels(&rect, stride, &mut pixels).ok()?;
        Some((pixels, sw, sh))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_is_none_without_duration() {
        let state = MediaState::default();
        assert_eq!(state.progress(), None);
    }

    #[test]
    fn progress_clamps_at_one() {
        let state = MediaState {
            position: Duration::from_secs(300),
            duration: Some(Duration::from_secs(100)),
            ..Default::default()
        };
        assert_eq!(state.progress(), Some(1.0));
    }

    #[test]
    fn zero_duration_is_not_a_division_by_zero() {
        let state = MediaState {
            position: Duration::from_secs(5),
            duration: Some(Duration::ZERO),
            ..Default::default()
        };
        assert_eq!(state.progress(), None);
    }

    #[test]
    fn has_session_needs_a_title_or_source() {
        assert!(!MediaState::default().has_session());
        assert!(MediaState {
            source: "Spotify.exe".into(),
            ..Default::default()
        }
        .has_session());
    }

    #[test]
    fn snapshot_is_default_before_first_poll() {
        // `start` must not block on WinRT, so this returns immediately.
        let media = Media::start();
        assert!(!media.snapshot().has_session());
    }
}
