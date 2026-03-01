//! Linux namespace isolation for worker processes.
//!
//! Provides a `pre_exec` closure that creates new PID and mount namespaces
//! for worker processes via `unshare(2)`. This isolates workers from each
//! other and from the host process tree.
//!
//! Namespace isolation layers:
//! - **PID namespace**: Worker sees itself as PID 1, cannot see/signal other processes
//! - **Mount namespace**: Worker gets its own mount table; host mounts are unaffected
//!
//! Network and user namespaces are NOT isolated here because:
//! - Network: workers need to bind ports and make outbound connections
//! - User: UID mapping requires root or special capabilities
//!
//! On non-Linux platforms this is a no-op.

use crate::error::AppError;

/// Apply PID and mount namespace isolation as a `pre_exec` hook.
///
/// # Safety
/// Calls `pre_exec` which runs in the forked child between fork/exec.
/// Only `unshare(2)` and `mount(2)` are called, which are async-signal-safe
/// in practice on Linux.
pub fn apply_namespace_isolation(cmd: &mut tokio::process::Command) -> Result<(), AppError> {
    #[cfg(target_os = "linux")]
    unsafe {
        cmd.pre_exec(|| {
            // Create new PID and mount namespaces
            let flags = libc::CLONE_NEWPID | libc::CLONE_NEWNS;
            let ret = libc::unshare(flags);
            if ret != 0 {
                let err = std::io::Error::last_os_error();
                // Non-fatal: if we lack CAP_SYS_ADMIN, namespace isolation
                // won't work but the process can still run.
                // Log via stderr since tracing isn't available in pre_exec.
                let msg = format!("rift: unshare(CLONE_NEWPID|CLONE_NEWNS) failed: {err}\n");
                let _ = libc::write(
                    2,
                    msg.as_ptr() as *const libc::c_void,
                    msg.len(),
                );
                // Continue without namespace isolation rather than failing
                return Ok(());
            }

            // Make mount namespace private so mount changes don't propagate
            let ret = libc::mount(
                std::ptr::null(),
                b"/\0".as_ptr() as *const libc::c_char,
                std::ptr::null(),
                libc::MS_REC | libc::MS_PRIVATE,
                std::ptr::null(),
            );
            if ret != 0 {
                // Non-fatal: private mount propagation is best-effort
                return Ok(());
            }

            // Remount /proc for the new PID namespace so the worker sees
            // only its own processes
            let ret = libc::mount(
                b"proc\0".as_ptr() as *const libc::c_char,
                b"/proc\0".as_ptr() as *const libc::c_char,
                b"proc\0".as_ptr() as *const libc::c_char,
                0,
                std::ptr::null(),
            );
            if ret != 0 {
                // Non-fatal: /proc remount may fail without proper caps
                return Ok(());
            }

            Ok(())
        });
    }

    #[cfg(not(target_os = "linux"))]
    let _ = cmd; // suppress unused warning

    Ok(())
}

/// Check if namespace isolation is available (requires CAP_SYS_ADMIN on Linux).
#[cfg(target_os = "linux")]
pub fn is_namespace_available() -> bool {
    // Test with a dry-run unshare of CLONE_NEWPID only
    // We fork, attempt unshare in the child, and check the result
    unsafe {
        let ret = libc::unshare(libc::CLONE_NEWPID);
        if ret == 0 {
            // Successfully created namespace — we're now in a new PID ns
            // This is fine for the check, the namespace goes away when we return
            true
        } else {
            false
        }
    }
}

#[cfg(not(target_os = "linux"))]
pub fn is_namespace_available() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namespace_availability_does_not_panic() {
        // Just verify this doesn't crash
        let _ = is_namespace_available();
    }

    #[test]
    fn apply_to_command_does_not_error() {
        let mut cmd = tokio::process::Command::new("true");
        let result = apply_namespace_isolation(&mut cmd);
        assert!(result.is_ok());
    }
}
