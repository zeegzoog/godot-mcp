use std::sync::{Mutex, MutexGuard, PoisonError};

/// Locks a mutex, recovering the data if another thread panicked while holding it.
///
/// The guarded maps in this crate stay structurally valid after a panic, so continuing is
/// safer than propagating the poison and taking the whole server down.
pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Whether a process with this id runs. It only reads the process table and sends no signal.
#[cfg(unix)]
pub fn process_runs(pid: u32) -> bool {
    let Ok(id) = i32::try_from(pid) else {
        return false;
    };
    if id <= 0 {
        return false;
    }
    // SAFETY: signal 0 checks that the process exists and delivers nothing.
    if unsafe { libc::kill(id, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(windows)]
pub fn process_runs(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_ACCESS_DENIED, GetLastError, WAIT_TIMEOUT,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
    };

    // SAFETY: the handle is checked before use and closed once; nothing else is touched.
    unsafe {
        let handle = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
        if handle.is_null() {
            return GetLastError() == ERROR_ACCESS_DENIED;
        }
        let running = WaitForSingleObject(handle, 0) == WAIT_TIMEOUT;
        CloseHandle(handle);
        running
    }
}
