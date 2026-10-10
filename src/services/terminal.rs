//! A ConPTY-backed shell: spawn the user's default shell inside a pseudo
//! console, pump its output into a [`Screen`], and forward keystrokes.
//!
//! # Why ConPTY and not pipes
//!
//! A bare pipe pair gives you a program that writes bytes, not a terminal: no
//! `isatty`, no cursor addressing, no colour detection, and every TUI (`pwsh`,
//! `lazygit`) collapses into raw escape sequences. ConPTY (`CreatePseudoConsole`)
//! is the OS's answer to that and is the only reason a terminal fits in an
//! overlay at all. It is three Win32 calls and no crates.
//!
//! # Shape
//!
//! [`Terminal`] owns the pseudo console and its pipes. A reader thread owns the
//! output read end and feeds bytes into the shared screen; nothing else touches
//! the pipe, so reads never race with the UI's frame draw. The app thread calls
//! [`Terminal::screen`] to get the latest grid snapshot.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Storage::FileSystem::{ReadFile, WriteFile};
use windows::Win32::System::Console::{
    ClosePseudoConsole, CreatePseudoConsole, HPCON, ResizePseudoConsole,
};
use windows::Win32::System::Pipes::CreatePipe;
use windows::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess,
    InitializeProcThreadAttributeList, TerminateProcess, UpdateProcThreadAttribute,
    CREATE_UNICODE_ENVIRONMENT, EXTENDED_STARTUPINFO_PRESENT, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE,
    PROCESS_INFORMATION, STARTUPINFOEXW, STARTUPINFOW,
};

use crate::core::termscreen::{Screen, COLS, ROWS};

/// An owned handle that closes itself once, on drop.
struct Handle(HANDLE);

impl Handle {
    fn new(h: HANDLE) -> Option<Self> {
        (!h.is_invalid()).then_some(Self(h))
    }
}

// An owned kernel handle is just an opaque index: moving it between threads is
// safe (it is not a pointer into this process's memory, and closing it from any
// thread is legal). `HANDLE` is `*mut c_void`, which the compiler cannot know.
unsafe impl Send for Handle {}

impl Drop for Handle {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// The pseudo console and the shell attached to it.
///
/// Dropping terminates the shell *first*, then closes the console. That order
/// is required: `ClosePseudoConsole` does not return until the client
/// detaches, so closing a console whose shell is still running hangs on drop.
///
/// `Option` because a failed spawn has no console to close, and a null `HPCON`
/// must never reach `ClosePseudoConsole`.
struct Pty {
    console: Option<HPCON>,
    /// The shell's process handle, so drop can kill it before closing the
    /// console. `None` on the spawn-failure path.
    process: Option<Handle>,
}

impl Drop for Pty {
    fn drop(&mut self) {
        if let Some(p) = self.process.take() {
            unsafe {
                let _ = TerminateProcess(p.0, 1);
            }
            // Close our process handle (dropped here, `p` moves out of self).
            drop(p);
        }
        if let Some(h) = self.console {
            unsafe { ClosePseudoConsole(h) }
        }
    }
}

// `HPCON` is a raw handle, same reasoning as `Handle`: the console object is
// owned, and closing it from any thread is legal.
unsafe impl Send for Pty {}

/// A live shell plus the grid it is painting into.
pub struct Terminal {
    screen: Arc<Mutex<Screen>>,
    /// Write end of the PTY: keystrokes go here. `None` when the spawn failed.
    input: Option<Handle>,
    pty: Arc<Pty>,
    /// The startup attribute list the child used to find the console. Must
    /// outlive the child — see [`AttrList`].
    _attrs: Option<AttrList>,
    /// The pipe ends the ConPTY itself references. Held until after the
    /// console is closed — Microsoft's sample keeps them open for the
    /// console's lifetime, and dropping them early can break the console's
    /// reader before it has drained. The reader thread owns the *other* read
    /// end (`out_r` moved into the reader closure).
    alive: Arc<AtomicBool>,
    /// Human-readable reason the spawn failed, shown in the panel instead of
    /// a blank grid.
    error: Option<String>,
}

impl Terminal {
    /// Spawn the default shell in a pseudo console sized to the grid.
    ///
    /// `ComSpec` (PowerShell or cmd) rather than probing for pwsh/git-bash: the
    /// registry's command processor is the one shell guaranteed present, and it
    /// is what a Windows user means by "open a terminal".
    pub fn new() -> Self {
        let screen = Arc::new(Mutex::new(Screen::new()));
        match Self::spawn(screen.clone()) {
            Ok(t) => t,
            Err(e) => {
                // A dead-but-valid terminal: the panel shows the error instead
                // of a blank grid, and every method stays a safe no-op.
                let screen = Arc::new(Mutex::new(Screen::new()));
                screen.lock().map(|mut g| g.feed(&[])).ok();
                Self {
                    screen,
                    input: None,
                    pty: Arc::new(Pty {
                        console: None,
                        process: None,
                    }),
                    _attrs: None,
                    alive: Arc::new(AtomicBool::new(false)),
                    error: Some(e),
                }
            }
        }
    }

    fn spawn(screen: Arc<Mutex<Screen>>) -> Result<Self, String> {
        // Inheritable ends: the child needs to inherit them, Arc needs to keep
        // the other ends.
        let sa = windows::Win32::Security::SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<windows::Win32::Security::SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: std::ptr::null_mut(),
            bInheritHandle: windows::core::BOOL(1),
        };

        let (in_r, in_w) = pipe_pair(&sa)?;
        let (out_r, out_w) = pipe_pair(&sa)?;

        let console = unsafe {
            CreatePseudoConsole(
                windows::Win32::System::Console::COORD {
                    X: COLS as i16,
                    Y: ROWS as i16,
                },
                in_r.0,
                out_w.0,
                0,
            )
            .map_err(|e| e.to_string())?
        };

        // The ConPTY kernel side dups these ends at creation (Alacritty does
        // the same), so ours can close now — keeping out_w open would stop
        // the reader ever seeing EOF when the shell exits.
        drop(out_w);
        drop(in_r);

        let mut cmd = shell_command()?;
        let (attrs, shell) = spawn_shell(&mut cmd, console)?;

        // The reader thread owns the read end for its whole life. A dead
        // shell breaks the pipe and this loop exits — no polling needed.
        let alive = Arc::new(AtomicBool::new(true));
        let (s, al) = (screen.clone(), alive.clone());
        std::thread::Builder::new()
            .name("conpty".into())
            .spawn(move || reader(s, al, out_r))
            .ok();

        Ok(Self {
            screen,
            input: Some(in_w),
            pty: Arc::new(Pty {
                console: Some(console),
                process: Some(shell),
            }),
            _attrs: Some(attrs),
            alive,
            error: None,
        })
    }

    /// A snapshot of the current grid for the UI.
    pub fn screen(&self) -> Screen {
        self.screen.lock().map(|g| g.clone()).unwrap_or_default()
    }

    /// Feed bytes into the shell's input.
    pub fn write(&self, bytes: &[u8]) -> bool {
        match &self.input {
            Some(h) => unsafe { WriteFile(h.0, Some(bytes), None, None).is_ok() },
            None => false,
        }
    }

    /// One printable keypress, as the terminal would receive it.
    pub fn key(&self, b: u8) -> bool {
        self.write(&[b])
    }

    /// Send a string and the Enter that submits it (used for the paste path).
    pub fn line(&self, s: &str) -> bool {
        self.write(s.as_bytes()) && self.write(b"\r")
    }

    /// Tell the PTY its viewport changed. Cheap, safe to call every frame.
    pub fn resize(&self, cols: u16, rows: u16) {
        let Some(h) = self.pty.console else {
            return;
        };
        unsafe {
            let _ = ResizePseudoConsole(
                h,
                windows::Win32::System::Console::COORD {
                    X: cols as i16,
                    Y: rows as i16,
                },
            );
        }
    }

    /// True while the shell is alive.
    ///
    /// Polls the process handle rather than the reader thread: conhost keeps
    /// its end of the output pipe open for its whole life, so the reader
    /// never sees EOF and its flag lags by one `ClosePseudoConsole`.
    pub fn alive(&self) -> bool {
        if !self.alive.load(Ordering::Relaxed) {
            return false;
        }
        let Some(p) = self.pty.process.as_ref() else {
            return false;
        };
        let mut code = 0u32;
        let ok = unsafe { GetExitCodeProcess(p.0, &mut code) }.is_ok();
        // 259 = STILL_ACTIVE: the process handle exists and the shell runs.
        ok && code == 259
    }

    /// Why the shell failed to start, if it did.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
}

/// The reader thread's whole job: drain the PTY into the shared screen until
/// the pipe breaks (shell exited) or [`Terminal`] is dropped.
///
/// A free function rather than a closure because a closure captures *fields*
/// under Rust 2021 disjoint capture — it would capture `reads.0` (a raw
/// `HANDLE`) and lose the `Send` impl on [`Handle`]. Taking the handle as an
/// argument keeps the whole `Handle` in the closure.
fn reader(screen: Arc<Mutex<Screen>>, alive: Arc<AtomicBool>, reads: Handle) {
    let mut buf = [0u8; 8192];
    while alive.load(Ordering::Relaxed) {
        let mut n = 0u32;
        let ok = unsafe { ReadFile(reads.0, Some(&mut buf), Some(&mut n), None) };
        if ok.is_err() || n == 0 {
            // A dead pipe means the shell exited: stop reading.
            break;
        }
        if let Ok(mut g) = screen.lock() {
            g.feed(&buf[..n as usize]);
        }
    }
    alive.store(false, Ordering::Relaxed);
}

/// Build a pipe pair with inheritable ends. Both handles are returned; the
/// caller decides which it keeps.
fn pipe_pair(sa: &windows::Win32::Security::SECURITY_ATTRIBUTES) -> Result<(Handle, Handle), String> {
    let (mut r, mut w) = (HANDLE::default(), HANDLE::default());
    unsafe {
        CreatePipe(&mut r, &mut w, Some(sa), 0).map_err(|e| e.code().0.to_string())?;
    }
    let r = Handle::new(r).ok_or_else(|| "null read pipe".to_string())?;
    let w = Handle::new(w).ok_or_else(|| "null write pipe".to_string())?;
    Ok((r, w))
}

/// The shell to run, as a mutable UTF-16 command line (CreateProcessW may
/// rewrite it in place).
fn shell_command() -> Result<Vec<u16>, String> {
    let shell = std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string());
    let mut cmd: Vec<u16> = format!("\"{}\"", shell).encode_utf16().collect();
    cmd.push(0);
    Ok(cmd)
}

/// A proc-thread attribute list that outlives process creation.
///
/// The child process reads its own startup attributes while it initializes —
/// that is how it finds the pseudoconsole. Freeing the list the moment
/// `CreateProcessW` returns races that read and the child dies with
/// STATUS_DLL_INIT_FAILED (0xC0000142), so the list is kept alive for as long
/// as the terminal exists.
struct AttrList {
    /// Pointer-aligned backing storage: the list is pointer-sized and
    /// `InitializeProcThreadAttributeList` requires "a buffer large enough to
    /// hold a pointer-aligned structure".
    buf: Box<[usize]>,
    list: windows::Win32::System::Threading::LPPROC_THREAD_ATTRIBUTE_LIST,
}

impl AttrList {
    /// Allocate and initialise a list with room for one attribute.
    fn new() -> Result<Self, String> {
        let mut size = 0usize;
        unsafe {
            // The NULL call always "fails" and reports the size it needs.
            let _ = InitializeProcThreadAttributeList(None, 1, Some(0), &mut size);
        }
        let buf = vec![0usize; size.div_ceil(std::mem::size_of::<usize>()) + 2];
        let mut buf: Box<[usize]> = buf.into_boxed_slice();
        let list = windows::Win32::System::Threading::LPPROC_THREAD_ATTRIBUTE_LIST(
            buf.as_mut_ptr() as _,
        );
        unsafe {
            InitializeProcThreadAttributeList(Some(list), 1, Some(0), &mut size)
                .map_err(|e| e.to_string())?;
        }
        Ok(Self { buf, list })
    }

    /// Point one attribute slot at the pseudoconsole `pty`.
    ///
    /// `lpValue` for `PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE` is the
    /// HPCON handle *itself* (the MS sample passes `hPC` directly;
    /// alacritty passes `pty_handle as *mut c_void`). Handing it a
    /// pointer to storage that merely holds the handle makes the
    /// kernel dereference our stack/heap address as a console handle:
    /// the child then fails to init with STATUS_DLL_INIT_FAILED
    /// (0xC0000142). The handle is just a number, so it needs no
    /// backing storage here.
    fn set_pseudoconsole(&mut self, pty: HPCON) -> Result<(), String> {
        unsafe {
            UpdateProcThreadAttribute(
                self.list,
                0,
                PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                Some(pty.0 as *mut core::ffi::c_void),
                std::mem::size_of::<isize>(),
                None,
                None,
            )
            .map_err(|e| e.to_string())
        }
    }
}

impl Drop for AttrList {
    fn drop(&mut self) {
        unsafe {
            DeleteProcThreadAttributeList(self.list);
        }
        let _ = &self.buf;
    }
}

/// Spawn the shell attached to the pseudo console.
///
/// The documented ConPTY path: an attribute list carrying
/// `PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE`, passed via `STARTUPINFOEXW`. The
/// child's stdio is *not* redirected — the console owns it.
///
/// Returns the attribute list to the caller, which must keep it alive for as
/// long as the child runs.
fn spawn_shell(cmd: &mut [u16], pty: HPCON) -> Result<(AttrList, Handle), String> {
    let mut attrs = AttrList::new()?;
    attrs.set_pseudoconsole(pty)?;

    let mut info: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
        info.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        // STARTF_USESTDHANDLES with all three std handles left null: the console
        // subsystem then fills them from the attached pseudoconsole. Without the
        // flag the child keeps whatever std handles this process had, so it prints
        // to *our* stdout and the grid never sees a byte.
        info.StartupInfo.dwFlags = windows::Win32::System::Threading::STARTF_USESTDHANDLES;
        info.lpAttributeList = attrs.list;

    let mut pi = PROCESS_INFORMATION::default();
    // bInheritHandles=false: the child reaches the console through the
    // attribute list, not through inherited handles.
    let r = unsafe {
        CreateProcessW(
            None,
            Some(windows::core::PWSTR(cmd.as_mut_ptr())),
            None,
            None,
            false,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
            None,
            None,
            &info.StartupInfo as *const STARTUPINFOW,
            &mut pi,
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
    };
    // The thread handle is ours to release; the process handle must outlive
        // the console so drop can terminate the shell first (see `Pty`).
        unsafe {
            let _ = CloseHandle(pi.hThread);
        }
        let process = Handle::new(pi.hProcess);
        r.map(|()| (attrs, process.expect("no process handle")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_error_path_yields_a_blank_screen_not_a_panic() {
        // A terminal whose shell failed to start must still answer `screen()`
        // with an empty grid rather than taking the app down.
        let t = Terminal::new();
        assert_eq!(t.screen().text(), "");
    }

    #[test]
    fn a_spawned_shell_actually_runs_and_paints_the_grid() {
        // The one test that matters: a real ConPTY, a real shell, real PTY
        // bytes through `vte` into the grid. If ConPTY plumbing is wrong the
        // screen stays blank and this fails.
        let t = Terminal::new();
        assert!(t.error().is_none(), "spawn failed: {:?}", t.error());
        assert!(t.alive(), "shell not alive right after spawn");

        // `echo` from cmd.exe: deterministic, exits fast, lands in the grid.
        assert!(t.line("echo arc_conpty_ok"));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut seen = false;
        while std::time::Instant::now() < deadline && !seen {
            seen = t.screen().text().contains("arc_conpty_ok");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(seen, "grid never showed the echo output; screen was:\n{}", t.screen().text());
    }

    #[test]
    fn typed_input_reaches_the_shell_and_can_kill_it() {
        // Round-trips through the input pipe: if `exit` ends the shell, the
        // pipe is wired in the right direction (this is what the ConPTY handle
        // bug looked like from the outside).
        let t = Terminal::new();
        assert!(t.error().is_none(), "spawn failed: {:?}", t.error());
        std::thread::sleep(std::time::Duration::from_millis(600));
        assert!(t.line("exit\r"), "write failed");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
        while std::time::Instant::now() < deadline && t.alive() {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        assert!(!t.alive(), "`exit` did not end the shell");
    }

    #[test]
    fn writes_to_a_dead_terminal_do_not_panic() {
        let t = Terminal::new();
        // Whatever the spawn did, feeding it input must be safe.
        let _ = t.key(b'x');
    }
}
