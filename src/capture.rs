//! Window-id lookup + screencapture subprocess for test PNGs. macOS only.

use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug)]
pub enum CaptureError {
    WindowNotFound,
    Io(std::io::Error),
    ScreencaptureFailed { status: Option<i32>, stderr: String },
}

impl std::fmt::Display for CaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WindowNotFound => write!(f, "no on-screen window found for this PID"),
            Self::Io(e) => write!(f, "io error: {}", e),
            Self::ScreencaptureFailed { status, stderr } => {
                write!(f, "screencapture exited with {:?}: {}", status, stderr)
            }
        }
    }
}

impl std::error::Error for CaptureError {}

impl From<std::io::Error> for CaptureError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

#[cfg(target_os = "macos")]
pub fn find_own_window_id() -> Result<u32, CaptureError> {
    use core_foundation::base::{CFType, TCFType};
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::number::CFNumber;
    use core_foundation::string::CFString;
    use core_graphics::window::{
        kCGNullWindowID, kCGWindowListOptionOnScreenOnly, kCGWindowNumber, kCGWindowOwnerPID,
        copy_window_info,
    };

    let pid = std::process::id() as i64;

    let info = copy_window_info(kCGWindowListOptionOnScreenOnly, kCGNullWindowID)
        .ok_or(CaptureError::WindowNotFound)?;

    // The array is untyped (CFArray<*const c_void>); each element is a CFDictionary.
    for i in 0..info.len() {
        // Safety: index is within bounds; each element is a CFDictionary<CFString, CFType>.
        let dict: CFDictionary<CFString, CFType> = unsafe {
            use core_foundation::array::CFArrayGetValueAtIndex;
            let raw = CFArrayGetValueAtIndex(info.as_concrete_TypeRef(), i as _);
            CFDictionary::wrap_under_get_rule(raw as _)
        };

        // Look up kCGWindowOwnerPID
        let owner_key = unsafe { CFString::wrap_under_get_rule(kCGWindowOwnerPID) };
        let Some(owner_val) = dict.find(&owner_key) else {
            continue;
        };
        let Some(owner_num) = owner_val.downcast::<CFNumber>() else {
            continue;
        };
        if owner_num.to_i64() != Some(pid) {
            continue;
        }

        // Look up kCGWindowNumber
        let num_key = unsafe { CFString::wrap_under_get_rule(kCGWindowNumber) };
        let Some(num_val) = dict.find(&num_key) else {
            continue;
        };
        let Some(num) = num_val.downcast::<CFNumber>() else {
            continue;
        };
        if let Some(id) = num.to_i64() {
            return Ok(id as u32);
        }
    }

    Err(CaptureError::WindowNotFound)
}

#[cfg(not(target_os = "macos"))]
pub fn find_own_window_id() -> Result<u32, CaptureError> {
    Err(CaptureError::WindowNotFound)
}

pub fn capture(window_id: u32, path: &Path) -> Result<PathBuf, CaptureError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let out = Command::new("screencapture")
        .arg("-l")
        .arg(window_id.to_string())
        .arg("-o")
        .arg("-x")
        .arg(path)
        .output()?;
    if !out.status.success() {
        return Err(CaptureError::ScreencaptureFailed {
            status: out.status.code(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    Ok(path.to_path_buf())
}
