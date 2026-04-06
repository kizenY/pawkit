//! Windows API helpers to find and focus Claude Code terminal windows.

#[cfg(target_os = "windows")]
extern "system" {
    fn EnumWindows(
        callback: unsafe extern "system" fn(isize, isize) -> i32,
        lparam: isize,
    ) -> i32;
    fn GetWindowTextW(hwnd: isize, text: *mut u16, max_count: i32) -> i32;
    fn IsWindowVisible(hwnd: isize) -> i32;
    fn SetForegroundWindow(hwnd: isize) -> i32;
    fn ShowWindow(hwnd: isize, cmd_show: i32) -> i32;
    fn GetForegroundWindow() -> isize;
}

#[cfg(target_os = "windows")]
const SW_RESTORE: i32 = 9;

#[cfg(target_os = "windows")]
fn get_window_title(hwnd: isize) -> String {
    let mut buf = [0u16; 512];
    let len = unsafe { GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
    if len > 0 {
        String::from_utf16_lossy(&buf[..len as usize])
    } else {
        String::new()
    }
}

#[cfg(target_os = "windows")]
unsafe extern "system" fn enum_callback(hwnd: isize, lparam: isize) -> i32 {
    if IsWindowVisible(hwnd) == 0 {
        return 1; // continue
    }
    let title = get_window_title(hwnd);
    let lower = title.to_lowercase();
    // Match windows whose title contains "claude"
    if lower.contains("claude") {
        *(lparam as *mut isize) = hwnd;
        return 0; // stop enumeration
    }
    1 // continue
}

/// Find a window with "claude" in its title and bring it to the foreground.
/// Returns true if a matching window was found and focused.
#[cfg(target_os = "windows")]
pub fn focus_claude_window() -> bool {
    let mut found: isize = 0;
    unsafe {
        EnumWindows(enum_callback, &mut found as *mut isize as isize);
        if found != 0 {
            ShowWindow(found, SW_RESTORE);
            SetForegroundWindow(found);
            true
        } else {
            false
        }
    }
}

/// Check if the currently focused (foreground) window has "claude" in its title.
#[cfg(target_os = "windows")]
pub fn is_claude_window_focused() -> bool {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd == 0 {
            return false;
        }
        let title = get_window_title(hwnd);
        title.to_lowercase().contains("claude")
    }
}

#[cfg(not(target_os = "windows"))]
pub fn focus_claude_window() -> bool {
    false
}

#[cfg(not(target_os = "windows"))]
pub fn is_claude_window_focused() -> bool {
    false
}
