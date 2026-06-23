#[cfg(windows)]
mod platform {
    use windows_sys::Win32::Foundation::{HWND, LPARAM};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, EnumWindows, GetWindowThreadProcessId, IsIconic, IsWindowVisible,
        SetForegroundWindow, ShowWindow, SW_RESTORE, SW_SHOW,
    };

    struct SearchState {
        process_id: u32,
        hwnd: HWND,
    }

    pub fn bring_process_to_front(process_id: u32) -> Result<(), String> {
        if process_id == 0 {
            return Err("invalid process id".to_string());
        }

        let mut state = SearchState {
            process_id,
            hwnd: 0 as HWND,
        };

        unsafe {
            EnumWindows(
                Some(enum_windows_for_process),
                (&mut state as *mut SearchState) as LPARAM,
            );
        }

        if state.hwnd == 0 as HWND {
            return Err(format!(
                "no visible top-level window found for PID {process_id}"
            ));
        }

        unsafe {
            if IsIconic(state.hwnd) != 0 {
                ShowWindow(state.hwnd, SW_RESTORE);
            } else {
                ShowWindow(state.hwnd, SW_SHOW);
            }
            BringWindowToTop(state.hwnd);
            if SetForegroundWindow(state.hwnd) == 0 {
                return Err(format!("SetForegroundWindow failed for PID {process_id}"));
            }
        }

        Ok(())
    }

    unsafe extern "system" fn enum_windows_for_process(hwnd: HWND, lparam: LPARAM) -> i32 {
        if IsWindowVisible(hwnd) == 0 {
            return 1;
        }

        let state = &mut *(lparam as *mut SearchState);
        let mut window_process_id = 0u32;
        GetWindowThreadProcessId(hwnd, &mut window_process_id);
        if window_process_id == state.process_id {
            state.hwnd = hwnd;
            return 0;
        }

        1
    }
}

#[cfg(windows)]
pub use platform::bring_process_to_front;

#[cfg(not(windows))]
pub fn bring_process_to_front(_process_id: u32) -> Result<(), String> {
    Err("foreground activation is only available on Windows".to_string())
}
