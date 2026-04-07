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
    fn keybd_event(bvk: u8, bscan: u8, dwflags: u32, dwextrainfo: usize);
}

#[cfg(target_os = "windows")]
const VK_MENU: u8 = 0x12; // Alt key
#[cfg(target_os = "windows")]
const KEYEVENTF_KEYUP: u32 = 0x0002;

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
extern "system" {
    fn GetWindowThreadProcessId(hwnd: isize, lpdwprocessid: *mut u32) -> u32;
}

#[cfg(target_os = "windows")]
fn is_windows_terminal(hwnd: isize) -> bool {
    let mut pid: u32 = 0;
    unsafe { GetWindowThreadProcessId(hwnd, &mut pid); }
    if pid == 0 { return false; }
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;

    extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> isize;
        fn CloseHandle(handle: isize) -> i32;
        fn QueryFullProcessImageNameW(process: isize, flags: u32, name: *mut u16, size: *mut u32) -> i32;
    }
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;

    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle == 0 { return false; }
        let mut buf = [0u16; 512];
        let mut size = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, 0, buf.as_mut_ptr(), &mut size);
        CloseHandle(handle);
        if ok == 0 { return false; }
        let name = OsString::from_wide(&buf[..size as usize]).to_string_lossy().to_lowercase();
        name.contains("windowsterminal")
    }
}

#[cfg(target_os = "windows")]
unsafe extern "system" fn enum_callback(hwnd: isize, lparam: isize) -> i32 {
    if IsWindowVisible(hwnd) == 0 {
        return 1; // continue
    }
    let title = get_window_title(hwnd);
    let lower = title.to_lowercase();
    // Match windows whose title contains "claude" or the hosting terminal process
    if lower.contains("claude") || is_windows_terminal(hwnd) {
        *(lparam as *mut isize) = hwnd;
        return 0; // stop enumeration
    }
    1 // continue
}

#[cfg(target_os = "windows")]
const SW_MINIMIZE: i32 = 6;

/// Find the terminal window and toggle it:
/// - If it's already the foreground window → minimize it
/// - Otherwise → bring it to the foreground
/// Returns true if a matching window was found.
#[cfg(target_os = "windows")]
pub fn focus_claude_window() -> bool {
    let mut found: isize = 0;
    unsafe {
        EnumWindows(enum_callback, &mut found as *mut isize as isize);
        if found != 0 {
            let fg = GetForegroundWindow();
            if fg == found {
                // Already focused — minimize
                ShowWindow(found, SW_MINIMIZE);
            } else {
                // Simulate Alt key press/release to release the foreground lock.
                keybd_event(VK_MENU, 0, 0, 0);
                keybd_event(VK_MENU, 0, KEYEVENTF_KEYUP, 0);
                ShowWindow(found, SW_RESTORE);
                SetForegroundWindow(found);
            }
            true
        } else {
            false
        }
    }
}

/// Check if the currently focused (foreground) window is the terminal.
#[cfg(target_os = "windows")]
pub fn is_claude_window_focused() -> bool {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd == 0 {
            return false;
        }
        let title = get_window_title(hwnd);
        title.to_lowercase().contains("claude") || is_windows_terminal(hwnd)
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
