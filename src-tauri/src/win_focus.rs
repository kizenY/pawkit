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
    fn GetCurrentThreadId() -> u32;
    fn AttachThreadInput(attach: u32, attach_to: u32, do_attach: i32) -> i32;
    fn IsIconic(hwnd: isize) -> i32;
    fn GetWindow(hwnd: isize, cmd: u32) -> isize;
    fn IsWindow(hwnd: isize) -> i32;
    fn GetWindowRect(hwnd: isize, rect: *mut [i32; 4]) -> i32;
}

#[cfg(target_os = "windows")]
const VK_MENU: u8 = 0x12; // Alt key
#[cfg(target_os = "windows")]
const VK_CONTROL: u8 = 0x11;
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
            toggle_window_focus(found);
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

/// Focus the terminal window associated with a specific Claude process PID.
/// Focus the terminal window for a specific Claude session.
/// Each session runs in its own terminal window (wt -w new).
/// Toggle: click once → show, click again → minimize.
/// Excludes Pawkit's own PID so headless child processes (auto_review)
/// don't accidentally toggle Pawkit's window.
#[cfg(target_os = "windows")]
pub fn focus_session_terminal(claude_pid: u32, is_same_session: bool) -> bool {
    let my_pid = std::process::id();
    let ancestors: Vec<u32> = get_ancestor_pids(claude_pid)
        .into_iter()
        .filter(|&pid| pid != my_pid)
        .collect();
    plog!("[Pawkit] focus_session_terminal: claude_pid={} is_same={}", claude_pid, is_same_session);
    if let Some(hwnd) = find_window_by_pid_set(&ancestors) {
        plog!("[Pawkit] focus_session_terminal: hwnd={}", hwnd);
        unsafe {
            let fg = GetForegroundWindow();
            if fg == hwnd && is_same_session {
                plog!("[Pawkit] focus_session_terminal: minimize");
                ShowWindow(hwnd, SW_MINIMIZE);
            } else {
                plog!("[Pawkit] focus_session_terminal: bring to front");
                let my_thread = GetCurrentThreadId();
                let mut target_pid: u32 = 0;
                let target_thread = GetWindowThreadProcessId(hwnd, &mut target_pid);
                AttachThreadInput(my_thread, target_thread, 1);
                ShowWindow(hwnd, SW_RESTORE);
                SetForegroundWindow(hwnd);
                AttachThreadInput(my_thread, target_thread, 0);
            }
        }
        return true;
    }
    plog!("[Pawkit] focus_session_terminal: no window found for pid={}", claude_pid);
    false
}

/// Toggle a window: if already foreground -> minimize, otherwise -> bring to front.
#[cfg(target_os = "windows")]
unsafe fn toggle_window_focus(hwnd: isize) {
    let fg = GetForegroundWindow();
    if fg == hwnd {
        ShowWindow(hwnd, SW_MINIMIZE);
    } else {
        let my_thread = GetCurrentThreadId();
        let mut pid: u32 = 0;
        let target_thread = GetWindowThreadProcessId(hwnd, &mut pid);
        AttachThreadInput(my_thread, target_thread, 1);
        ShowWindow(hwnd, SW_RESTORE);
        SetForegroundWindow(hwnd);
        AttachThreadInput(my_thread, target_thread, 0);
    }
}

/// Determine which tab index (0-based) in Windows Terminal hosts the given Claude process.
/// Returns None if the window isn't Windows Terminal or only has one tab.
#[cfg(target_os = "windows")]
fn find_tab_index(hwnd: isize, claude_pid: u32) -> Option<usize> {
    if !is_windows_terminal(hwnd) {
        return None;
    }

    let mut terminal_pid: u32 = 0;
    unsafe { GetWindowThreadProcessId(hwnd, &mut terminal_pid); }
    if terminal_pid == 0 { return None; }

    extern "system" {
        fn CreateToolhelp32Snapshot(flags: u32, process_id: u32) -> isize;
        fn Process32FirstW(snapshot: isize, pe: *mut TabProcEntry) -> i32;
        fn Process32NextW(snapshot: isize, pe: *mut TabProcEntry) -> i32;
        fn CloseHandle(handle: isize) -> i32;
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> isize;
        fn GetProcessTimes(
            process: isize,
            creation: *mut [u32; 2],
            exit: *mut [u32; 2],
            kernel: *mut [u32; 2],
            user: *mut [u32; 2],
        ) -> i32;
    }

    #[repr(C)]
    struct TabProcEntry {
        dw_size: u32,
        cnt_usage: u32,
        th32_process_id: u32,
        th32_default_heap_id: usize,
        th32_module_id: u32,
        cnt_threads: u32,
        th32_parent_process_id: u32,
        pc_pri_class_base: i32,
        dw_flags: u32,
        sz_exe_file: [u16; 260],
    }

    const TH32CS_SNAPPROCESS: u32 = 0x00000002;
    const INVALID_HANDLE: isize = -1;
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;

    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE { return None; }

        let mut pe: TabProcEntry = std::mem::zeroed();
        pe.dw_size = std::mem::size_of::<TabProcEntry>() as u32;

        // Shell process names that represent actual terminal tabs
        const TAB_SHELLS: &[&str] = &["cmd.exe", "bash.exe", "powershell.exe", "pwsh.exe", "wsl.exe", "ubuntu"];

        fn is_shell_process(exe_file: &[u16; 260]) -> bool {
            let len = exe_file.iter().position(|&c| c == 0).unwrap_or(260);
            let name = String::from_utf16_lossy(&exe_file[..len]).to_lowercase();
            TAB_SHELLS.iter().any(|s| name.contains(s))
        }

        // Collect direct shell children of terminal_pid + build parent map
        let mut children: Vec<u32> = vec![];
        let mut parent_map = std::collections::HashMap::new();

        if Process32FirstW(snapshot, &mut pe) != 0 {
            parent_map.insert(pe.th32_process_id, pe.th32_parent_process_id);
            if pe.th32_parent_process_id == terminal_pid && is_shell_process(&pe.sz_exe_file) {
                children.push(pe.th32_process_id);
            }
            while Process32NextW(snapshot, &mut pe) != 0 {
                parent_map.insert(pe.th32_process_id, pe.th32_parent_process_id);
                if pe.th32_parent_process_id == terminal_pid && is_shell_process(&pe.sz_exe_file) {
                    children.push(pe.th32_process_id);
                }
            }
        }
        CloseHandle(snapshot);

        plog!("[Pawkit] find_tab_index: terminal_pid={} shell_children={:?}", terminal_pid, children);

        // Only one child → no tab switching needed
        if children.len() <= 1 { return None; }

        // Sort children by creation time to determine tab order
        let mut children_with_time: Vec<(u32, u64)> = children.iter()
            .filter_map(|&pid| {
                let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
                if handle == 0 { return None; }
                let mut creation = [0u32; 2];
                let mut exit = [0u32; 2];
                let mut kernel = [0u32; 2];
                let mut user = [0u32; 2];
                let ok = GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user);
                CloseHandle(handle);
                if ok == 0 { return None; }
                let time = ((creation[1] as u64) << 32) | (creation[0] as u64);
                Some((pid, time))
            })
            .collect();
        children_with_time.sort_by_key(|&(_, t)| t);

        // Walk up from claude_pid to find which child of terminal is its ancestor
        let mut current = claude_pid;
        let mut target_child = None;
        for _ in 0..20 {
            if children_with_time.iter().any(|&(pid, _)| pid == current) {
                target_child = Some(current);
                break;
            }
            if let Some(&parent) = parent_map.get(&current) {
                if parent == 0 || parent == current { break; }
                current = parent;
            } else {
                break;
            }
        }

        let result = target_child.and_then(|target| {
            children_with_time.iter().position(|&(pid, _)| pid == target)
        });
        plog!("[Pawkit] find_tab_index: sorted_children={:?} target_child={:?} result={:?}",
            children_with_time.iter().map(|&(pid, _)| pid).collect::<Vec<_>>(),
            target_child, result);
        result
    }
}

/// Switch Windows Terminal to tab N (0-based index) via SendInput (Ctrl+Alt+N).
/// Requires switchToTab keybindings in WT settings.
#[cfg(target_os = "windows")]
fn send_tab_switch_input(tab_index: usize) {
    if tab_index >= 9 { return; }
    plog!("[Pawkit] send_tab_switch_input: Ctrl+Alt+{}", tab_index + 1);

    extern "system" {
        fn SendInput(count: u32, inputs: *const RawInput, size: i32) -> u32;
    }

    // INPUT struct layout on x64: type(4) + pad(4) + union(32) = 40 bytes
    // Union sized to MOUSEINPUT (largest variant)
    #[repr(C)]
    struct RawInput {
        type_: u32,        // offset 0
        _pad0: u32,        // offset 4 (alignment)
        vk: u16,           // offset 8 (KEYBDINPUT.wVk)
        scan: u16,         // offset 10
        flags: u32,        // offset 12
        time: u32,         // offset 16
        _pad1: u32,        // offset 20 (align extra_info)
        extra_info: usize, // offset 24
        _pad2: [u8; 8],    // offset 32 (pad union to 32 bytes)
    }

    fn key(vk: u16, flags: u32) -> RawInput {
        RawInput {
            type_: 1, // INPUT_KEYBOARD
            _pad0: 0,
            vk,
            scan: 0,
            flags,
            time: 0,
            _pad1: 0,
            extra_info: 0,
            _pad2: [0; 8],
        }
    }

    let vk_num = 0x31 + tab_index as u16; // VK_1 = 0x31
    let inputs = [
        key(0x11, 0),                // Ctrl down
        key(0x12, 0),                // Alt down
        key(vk_num, 0),              // Number down
        key(vk_num, 0x0002),         // Number up (KEYEVENTF_KEYUP)
        key(0x12, 0x0002),           // Alt up
        key(0x11, 0x0002),           // Ctrl up
    ];

    plog!("[Pawkit] RawInput size: {}", std::mem::size_of::<RawInput>());
    let sent = unsafe {
        SendInput(6, inputs.as_ptr(), std::mem::size_of::<RawInput>() as i32)
    };
    plog!("[Pawkit] send_tab_switch_input: sent {} of 6 events", sent);
}

/// Walk up the process tree from a PID, collecting ancestor PIDs.
#[cfg(target_os = "windows")]
pub fn get_ancestor_pids(target_pid: u32) -> Vec<u32> {
    extern "system" {
        fn CreateToolhelp32Snapshot(flags: u32, process_id: u32) -> isize;
        fn Process32FirstW(snapshot: isize, pe: *mut ProcessEntry32W) -> i32;
        fn Process32NextW(snapshot: isize, pe: *mut ProcessEntry32W) -> i32;
        fn CloseHandle(handle: isize) -> i32;
    }

    #[repr(C)]
    struct ProcessEntry32W {
        dw_size: u32,
        cnt_usage: u32,
        th32_process_id: u32,
        th32_default_heap_id: usize,
        th32_module_id: u32,
        cnt_threads: u32,
        th32_parent_process_id: u32,
        pc_pri_class_base: i32,
        dw_flags: u32,
        sz_exe_file: [u16; 260],
    }

    const TH32CS_SNAPPROCESS: u32 = 0x00000002;
    const INVALID_HANDLE: isize = -1;

    let mut result = vec![target_pid];

    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE {
            return result;
        }

        // Build pid->parent map
        let mut parent_map = std::collections::HashMap::new();
        let mut pe: ProcessEntry32W = std::mem::zeroed();
        pe.dw_size = std::mem::size_of::<ProcessEntry32W>() as u32;

        if Process32FirstW(snapshot, &mut pe) != 0 {
            parent_map.insert(pe.th32_process_id, pe.th32_parent_process_id);
            while Process32NextW(snapshot, &mut pe) != 0 {
                parent_map.insert(pe.th32_process_id, pe.th32_parent_process_id);
            }
        }

        CloseHandle(snapshot);

        // Walk up from target_pid
        let mut current = target_pid;
        for _ in 0..20 {
            if let Some(&parent) = parent_map.get(&current) {
                if parent == 0 || parent == current {
                    break;
                }
                result.push(parent);
                current = parent;
            } else {
                break;
            }
        }
    }

    result
}

/// Cached terminal HWNDs per PID set — avoids picking different
/// XAML helper windows on each EnumWindows call.
#[cfg(target_os = "windows")]
static CACHED_HWNDS: std::sync::Mutex<Option<std::collections::HashMap<u32, isize>>> =
    std::sync::Mutex::new(None);

/// Find the main window whose owning process is in the given PID set.
/// Caches the result so we always use the same HWND.
#[cfg(target_os = "windows")]
fn find_window_by_pid_set(pids: &[u32]) -> Option<isize> {
    // Check cache first — reuse if the window still exists
    // Use the first PID in the set as cache key (the target process PID)
    let cache_key = pids.first().copied().unwrap_or(0);
    if let Ok(guard) = CACHED_HWNDS.lock() {
        if let Some(ref map) = *guard {
            if let Some(&cached) = map.get(&cache_key) {
                if unsafe { IsWindow(cached) } != 0 {
                    let mut pid: u32 = 0;
                    unsafe { GetWindowThreadProcessId(cached, &mut pid); }
                    if pids.contains(&pid) {
                        return Some(cached);
                    }
                }
            }
        }
    }

    // Scan for all candidate windows, pick the largest (= main window)
    struct CallbackData {
        pids: Vec<u32>,
        candidates: Vec<isize>,
    }

    unsafe extern "system" fn callback(hwnd: isize, lparam: isize) -> i32 {
        // Include both visible AND minimized (iconic) windows
        if IsWindowVisible(hwnd) == 0 && IsIconic(hwnd) == 0 {
            return 1;
        }
        let data = &mut *(lparam as *mut CallbackData);
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if data.pids.contains(&pid) {
            let title = get_window_title(hwnd);
            if !title.is_empty() {
                data.candidates.push(hwnd);
            }
        }
        1 // continue (collect all)
    }

    let mut data = CallbackData {
        pids: pids.to_vec(),
        candidates: vec![],
    };
    unsafe {
        EnumWindows(callback, &mut data as *mut CallbackData as isize);
    }

    // Pick the window with the largest area (main window is largest)
    let best = data.candidates.iter().copied().max_by_key(|&hwnd| {
        let mut rect = [0i32; 4]; // left, top, right, bottom
        if unsafe { GetWindowRect(hwnd, &mut rect) } != 0 {
            let w = (rect[2] - rect[0]).abs() as i64;
            let h = (rect[3] - rect[1]).abs() as i64;
            w * h
        } else {
            0
        }
    });

    if let Some(hwnd) = best {
        plog!("[Pawkit] find_window_by_pid_set: picked hwnd={} from {} candidates", hwnd, data.candidates.len());
        if let Ok(mut guard) = CACHED_HWNDS.lock() {
            let map = guard.get_or_insert_with(std::collections::HashMap::new);
            map.insert(cache_key, hwnd);
        }
        Some(hwnd)
    } else {
        None
    }
}

#[cfg(not(target_os = "windows"))]
pub fn focus_session_terminal(_claude_pid: u32, _is_same_session: bool) -> bool {
    false
}

#[cfg(not(target_os = "windows"))]
pub fn focus_claude_window() -> bool {
    false
}

#[cfg(not(target_os = "windows"))]
pub fn is_claude_window_focused() -> bool {
    false
}

#[cfg(not(target_os = "windows"))]
pub fn get_ancestor_pids(_target_pid: u32) -> Vec<u32> {
    vec![_target_pid]
}
