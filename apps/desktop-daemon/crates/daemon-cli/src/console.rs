//! Console output for CLI subcommands.
//!
//! The binary is compiled with `windows_subsystem = "windows"` so the
//! GUI daemon never flashes a console. That also detaches stdout from
//! any parent terminal — plain `println!` from a subcommand would
//! silently vanish. This module re-attaches to the parent console on
//! Windows and writes through the console's standard handles; on other
//! platforms it falls back to stdout/stderr directly.

/// Print a line to the attached console (subcommand output).
#[macro_export]
macro_rules! cprintln {
    ($($arg:tt)*) => {
        $crate::console::out(&format!("{}\n", format_args!($($arg)*)))
    };
}

/// Print a line to the attached console's stderr.
#[macro_export]
macro_rules! ceprintln {
    ($($arg:tt)*) => {
        $crate::console::err(&format!("{}\n", format_args!($($arg)*)))
    };
}

#[cfg(windows)]
pub fn attach() {
    use windows_sys::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
    unsafe {
        // Best effort: when launched from a terminal, attach to it so
        // the console handles below resolve. When launched from an
        // explorer/double-click, this fails and output is dropped.
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

#[cfg(not(windows))]
pub fn attach() {}

#[cfg(windows)]
fn console_file(name: &str) -> Option<std::fs::File> {
    use std::io::Write;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::FromRawHandle;
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    const GENERIC_READ: u32 = 0x8000_0000;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    // CONOUT$/CONERR$ bypass the subsystem detachment: with
    // windows_subsystem=windows, GetStdHandle returns NULL even after
    // AttachConsole, but the console device names still resolve to the
    // parent console.
    let wide: Vec<u16> = std::ffi::OsStr::new(name).encode_wide().chain(std::iter::once(0)).collect();
    unsafe {
        let handle = CreateFileW(
            wide.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null_mut(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        );
        if handle == INVALID_HANDLE_VALUE {
            return None;
        }
        Some(std::fs::File::from_raw_handle(handle as _))
    }
}

#[cfg(windows)]
fn inherited_std_handle(which: u32) -> Option<std::fs::File> {
    use std::os::windows::io::FromRawHandle;
    use windows_sys::Win32::System::Threading::{GetStartupInfoW, STARTUPINFOW};

    // The CRT nulls std handles for /SUBSYSTEM:WINDOWS binaries, but
    // GetStartupInfoW preserves the handles the parent passed — real
    // console handles OR pipes (the agent/CI case). This is how GUI
    // binaries recover piped stdio.
    let mut si = std::mem::MaybeUninit::<STARTUPINFOW>::uninit();
    unsafe {
        GetStartupInfoW(si.as_mut_ptr());
        let si = si.assume_init();
        let handle = match which {
            0 => si.hStdOutput,
            1 => si.hStdError,
            _ => si.hStdInput,
        };
        if handle.is_null() {
            return None;
        }
        Some(std::fs::File::from_raw_handle(handle as _))
    }
}

#[cfg(windows)]
pub fn out(s: &str) {
    use std::io::Write;
    if let Some(mut f) = inherited_std_handle(0).or_else(|| console_file("CONOUT$")) {
        let _ = f.write_all(s.as_bytes());
        let _ = f.flush();
        return;
    }
}

#[cfg(not(windows))]
pub fn out(s: &str) {
    print!("{s}");
}

#[cfg(windows)]
pub fn err(s: &str) {
    use std::io::Write;
    if let Some(mut f) = inherited_std_handle(1).or_else(|| console_file("CONERR$")) {
        let _ = f.write_all(s.as_bytes());
        let _ = f.flush();
        return;
    }
}

#[cfg(not(windows))]
pub fn err(s: &str) {
    eprint!("{s}");
}
