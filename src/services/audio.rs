//! Audio endpoint poller: master volume, mute, and a mic-in-use watchdog.
//!
//! One worker thread owns the COM pointers (MTA apartment of its own, same
//! pattern as the media service): the poll cadence is 250 ms, which is under
//! the plan's 0.5 s budget and still effectively free. There is no callback
//! path — `IAudioEndpointVolume` does have an event interface, but implementing
//! `IMMNotificationClient` in rust/winrt for one slider would be a lot of COM
//! vtable for a value we can read in a microsecond.
//!
//! Device switches (headphones plugged in) surface as failing `get` calls: the
//! service then re-resolves the default endpoint on the next tick.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::core::GUID;
use windows::Win32::Media::Audio::Endpoints::{IAudioEndpointVolume, IAudioMeterInformation};
use windows::Win32::Media::Audio::{eCapture, eConsole, eRender, IMMDevice, IMMDeviceEnumerator};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};

/// Poll cadence. Volume-key repeats land inside one tick; the mic watchdog
/// wants a sampling interval shorter than the speech pauses it must bridge.
const POLL: Duration = Duration::from_millis(250);

/// Peak above which the mic counts as "in use" (plan: hysteresis).
const MIC_ON_PEAK: f32 = 0.01;
/// And it stays "in use" this long after the last above-threshold peak, so
/// between-word gaps do not blink the dot.
const MIC_HOLD: Duration = Duration::from_secs(2);

/// The CLSID isn't vendored by the `windows` crate; this GUID is public and
/// fixed for ever (mmdevapi.dll).
const MMDEVICE_ENUMERATOR_CLSID: GUID = GUID::from_u128(0xBCDE0395_E52F_467C_8E3D_C4579291692E);

/// Latest audio reading. All fields are `None` until the first successful poll.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct AudioState {
    /// Master volume 0..=100.
    pub volume: Option<u8>,
    pub muted: Option<bool>,
    /// Mic capturing above threshold, held through short gaps.
    pub mic_active: bool,
    /// Output peak 0..=1, the render endpoint's last reading. `None` until the
    /// first successful poll; the visualizer chases this.
    pub peak: Option<f32>,
}

/// A write the app thread hands to the worker: the COM endpoint pointers live
/// on the worker's MTA apartment, so a setter only stores the request and the
/// next tick applies it (~250 ms, inside the plan's latency budget and free).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmd {
    /// Master volume 0..=100.
    Volume(u8),
    Mute(bool),
}

pub struct Audio {
    shared: Arc<Mutex<AudioState>>,
    cmd: Arc<Mutex<Option<Cmd>>>,
}

impl Audio {
    /// Spawn the poller. Like the media service: a machine with no audio
    /// endpoint gets a thread publishing `None`s, never a failed startup.
    pub fn start() -> Self {
        let shared = Arc::new(Mutex::new(AudioState::default()));
        let worker = shared.clone();
        let cmd = Arc::new(Mutex::new(None as Option<Cmd>));
        std::thread::Builder::new()
            .name("arc-audio".into())
            .spawn({
                let cmd = cmd.clone();
                move || run(worker, cmd)
            })
            .ok();
        Self { shared, cmd }
    }

    /// Set master volume 0..=100; applied on the worker's next tick.
    pub fn set_volume(&self, pct: u8) {
        self.write(Cmd::Volume(pct.min(100)));
    }

    /// Mute or unmute the default output; applied on the worker's next tick.
    pub fn set_mute(&self, mute: bool) {
        self.write(Cmd::Mute(mute));
    }

    fn write(&self, cmd: Cmd) {
        if let Ok(mut slot) = self.cmd.lock() {
            *slot = Some(cmd);
        }
    }

    /// Current reading. Cheap: a mutex read.
    pub fn snapshot(&self) -> AudioState {
        self.shared.lock().map(|s| *s).unwrap_or_default()
    }
}

fn run(shared: Arc<Mutex<AudioState>>, cmd: Arc<Mutex<Option<Cmd>>>) {
        crate::app::dbg_spawn_log("audio.rs");
    let mut out_meter: Option<IAudioMeterInformation> = None;
    // SAFETY: MTA apartment for this thread alone; the media service uses the
    // same model. Refusing audio (audio-less session) is fine.
    crate::app::dbg_spawn_log("audio:entry");
    let _com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.ok();
    crate::app::dbg_spawn_log("audio:com");

    let enumerator: windows::core::Result<IMMDeviceEnumerator> =
        unsafe { CoCreateInstance(&MMDEVICE_ENUMERATOR_CLSID, None, CLSCTX_INPROC_SERVER) };
    let Ok(enumerator) = enumerator else {
        return; // no COM audio at all: publish nothing, stay quiet
    };

    let mut volume: Option<(IMMDevice, IAudioEndpointVolume)> = None;
    let mut meter: Option<(IMMDevice, IAudioMeterInformation)> = None;
    let mut mic_active_until: Option<Instant> = None;

    crate::app::dbg_spawn_log("audio:loop");
    loop {
        std::thread::sleep(POLL);
        let now = Instant::now();

        let mut state = AudioState::default();
        state.mic_active = mic_active_until.is_some_and(|t| now < t);

        // Apply one pending write, if any, against the endpoint we hold.
        let pending = cmd.lock().ok().and_then(|slot| *slot);
        if let Some(c) = pending {
            if let Some((_, v)) = volume.as_ref() {
                // SAFETY: setters on a live endpoint, same object as reads.
                match c {
                    Cmd::Volume(p) => unsafe {
                        let _ = v.SetMasterVolumeLevelScalar(p as f32 / 100.0, std::ptr::null());
                    },
                    Cmd::Mute(m) => unsafe {
                        let _ = v.SetMute(m, std::ptr::null());
                    },
                }
            }
        }

        // Output: volume + mute. `Activate` re-resolves nothing, so a dead
        // device (unplugged headphones) fails the get and forces a re-acquire.
        if volume.as_ref().and_then(|(_, v)| read_volume(v)).is_some() {
            // happy path below
        } else {
            volume = default_endpoint(&enumerator, eRender, eConsole).and_then(|device| {
                let v = activate_endpoint_volume(&device).ok()?;
                Some((device, v))
            });
        }
        if let Some((_, v)) = volume.as_ref() {
            if let Some((vol, muted)) = read_volume(v) {
                state.volume = Some(vol);
                state.muted = Some(muted);
            }
        }

        // Output: peak meter for the visualizer. The volume endpoint already
        // holds the render device, so activating the meter on it is one call;
        // re-acquired together with the volume endpoint when it dies.
        if let Some((device, _)) = volume.as_ref() {
            if out_meter.is_none() {
                // SAFETY: in-proc activation of the standard meter interface.
                out_meter = unsafe {
                    device.Activate::<IAudioMeterInformation>(CLSCTX_INPROC_SERVER, None)
                }
                .ok();
            }
            if let Some(m) = out_meter.as_ref() {
                // SAFETY: plain out-param read.
                if let Ok(p) = unsafe { m.GetPeakValue() } {
                    state.peak = Some(p);
                }
            }
        }

        // Input: peak meter. Same re-acquire-on-failure rule.
        let peak = meter
            .as_ref()
            .and_then(|(_, m)| unsafe { m.GetPeakValue() }.ok());
        let peak = match peak {
            Some(p) => Some(p),
            None => {
                meter = default_endpoint(&enumerator, eCapture, eConsole).and_then(|device| {
                    // SAFETY: in-proc activation of the standard meter interface.
                    let m = unsafe {
                        device.Activate::<IAudioMeterInformation>(CLSCTX_INPROC_SERVER, None)
                    }
                    .ok()?;
                    Some((device, m))
                });
                meter
                    .as_ref()
                    .and_then(|(_, m)| unsafe { m.GetPeakValue() }.ok())
            }
        };
        if let Some(p) = peak {
            if p > MIC_ON_PEAK {
                mic_active_until = Some(now + MIC_HOLD);
                state.mic_active = true;
            }
        }

        if let Ok(mut s) = shared.lock() {
            *s = state;
        }
    }
}

fn default_endpoint(
    enumerator: &IMMDeviceEnumerator,
    flow: windows::Win32::Media::Audio::EDataFlow,
    role: windows::Win32::Media::Audio::ERole,
) -> Option<IMMDevice> {
    // SAFETY: plain COM getter on a live enumerator.
    unsafe { enumerator.GetDefaultAudioEndpoint(flow, role) }.ok()
}

fn activate_endpoint_volume(device: &IMMDevice) -> windows::core::Result<IAudioEndpointVolume> {
    // SAFETY: standard device activation; CLSCTX in-proc is what the endpoint
    // supports.
    unsafe { device.Activate::<IAudioEndpointVolume>(CLSCTX_INPROC_SERVER, None) }
}

fn read_volume(v: &IAudioEndpointVolume) -> Option<(u8, bool)> {
    // SAFETY: scalar getters on a live endpoint.
    let level = unsafe { v.GetMasterVolumeLevelScalar() }.ok()?;
    let muted = unsafe { v.GetMute() }
        .ok()
        .map(|b| b.as_bool())
        .unwrap_or(false);
    Some((((level * 100.0).round() as u8).min(100), muted))
}
