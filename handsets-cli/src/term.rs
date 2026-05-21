// Shared terminal-size probe used by both `screen` (one-shot text layout) and
// `mirror` (continuous text video). Pure FFI to `ioctl(TIOCGWINSZ)` so the
// crate stays dep-free apart from `zune-jpeg`.

#[repr(C)]
struct Winsize {
    ws_row: u16,
    ws_col: u16,
    ws_xpixel: u16,
    ws_ypixel: u16,
}

#[cfg(target_os = "macos")]
const TIOCGWINSZ: u64 = 0x4008_7468;
#[cfg(target_os = "linux")]
const TIOCGWINSZ: u64 = 0x5413;
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
const TIOCGWINSZ: u64 = 0x5413;

extern "C" {
    fn ioctl(fd: i32, request: u64, ws: *mut Winsize) -> i32;
}

/// `(cols, rows)` of the terminal attached to stdout, or `None` if stdout
/// isn't a TTY (e.g., when piped to a file or another process).
pub fn term_size() -> Option<(u16, u16)> {
    let mut ws = Winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let ret = unsafe { ioctl(1, TIOCGWINSZ, &mut ws) };
    if ret == -1 || ws.ws_col == 0 || ws.ws_row == 0 {
        return None;
    }
    Some((ws.ws_col, ws.ws_row))
}
