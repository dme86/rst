//! Native executable-name lookup for the foreground process group of a PTY.
//! Runs only during a throttled probe. Never invokes ps, shell commands,
//! traverses the global process list, or reads another tab's PTY data.

/// Return the executable of the supplied process, not the typed shell alias.
/// A process-group leader is normally the foreground job's first executable.
pub fn executable_name(pid: i32) -> Option<String> {
    if pid <= 0 {
        return None;
    }
    let name = platform_name(pid)?;
    crate::tab_naming::program_from_osc133(name.as_bytes())
}

#[cfg(target_os = "linux")]
fn platform_name(pid: i32) -> Option<String> {
    // /proc/<pid>/comm is the kernel task name, unlike /proc/<pid>/cmdline
    // which may contain the command as typed and its argument vector.
    // This is a single small read from a known PID, not a /proc scan.
    let raw = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    let name = raw.trim_end_matches('\n');
    (!name.is_empty()).then(|| name.to_owned())
}

#[cfg(target_os = "macos")]
fn platform_name(pid: i32) -> Option<String> {
    use std::ffi::c_void;

    // libproc is a system library provided by macOS. proc_pidpath reports
    // the binary the kernel actually executed, even if the shell used an
    // alias. proc_name is a fallback for processes without an accessible path.
    #[link(name = "proc")]
    extern "C" {
        fn proc_pidpath(pid: i32, buffer: *mut c_void, buffersize: u32) -> i32;
        fn proc_name(pid: i32, buffer: *mut c_void, buffersize: u32) -> i32;
    }
    let mut buffer = [0u8; 4096];
    let mut written = unsafe {
        proc_pidpath(pid, buffer.as_mut_ptr().cast(), buffer.len() as u32)
    };
    if written <= 0 {
        buffer.fill(0);
        written = unsafe {
            proc_name(pid, buffer.as_mut_ptr().cast(), buffer.len() as u32)
        };
    }
    if written <= 0 {
        return None;
    }
    let end = buffer.iter().position(|b| *b == 0).unwrap_or(written as usize);
    let path = std::str::from_utf8(buffer.get(..end)?).ok()?;
    Some(path.rsplit('/').next()?.to_owned())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn platform_name(_pid: i32) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_process_ids_do_not_resolve() {
        assert_eq!(executable_name(-1), None);
        assert_eq!(executable_name(0), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn unknown_linux_pid_is_not_an_executable() {
        assert_eq!(executable_name(i32::MAX), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_comm_looks_up_real_kernel_process_name() {
        let current = std::process::id() as i32;
        let result = executable_name(current);
        assert!(result.is_some(), "own process must have a readable comm");
    }
}
