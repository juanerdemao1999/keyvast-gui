//! Free-disk-space query for the recording headroom indicator (#13).
//!
//! Windows-only via `GetDiskFreeSpaceExW`; other platforms return `None` so
//! the UI simply omits the headroom line.

use std::path::PathBuf;

/// Bytes available to the caller on the volume that holds `path`.
///
/// `path` may be relative or point at a directory that doesn't exist yet (the
/// usual case before the first recording) — we resolve it to the nearest
/// existing ancestor so the query still reflects the right volume.
#[cfg(windows)]
pub fn free_bytes(path: &str) -> Option<u64> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    let mut p = PathBuf::from(path);
    if p.is_relative()
        && let Ok(cwd) = std::env::current_dir()
    {
        p = cwd.join(p);
    }
    while !p.exists() {
        match p.parent() {
            Some(parent) => p = parent.to_path_buf(),
            None => break,
        }
    }

    let wide: Vec<u16> = OsStr::new(p.as_os_str())
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let mut free_to_caller: u64 = 0;
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut free_to_caller,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if ok != 0 { Some(free_to_caller) } else { None }
}

#[cfg(not(windows))]
pub fn free_bytes(_path: &str) -> Option<u64> {
    None
}
