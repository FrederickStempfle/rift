//! Process-level seccomp BPF enforcement.
//!
//! Provides a `pre_exec` closure that applies seccomp filtering directly inside
//! the child process before it execs into Deno/Node. This is a second layer of
//! defense on top of the Docker-level seccomp profile.
//!
//! On Linux, the filter is applied via:
//! 1. `prctl(PR_SET_NO_NEW_PRIVS, 1)` — required before seccomp can be installed
//! 2. `seccomp(SECCOMP_SET_MODE_STRICT_FILTER, 0, &prog)` — install a BPF program
//!
//! On non-Linux platforms this is a no-op (returns success).

use std::path::Path;

use crate::error::AppError;

/// Apply seccomp profile as a `pre_exec` hook on a `tokio::process::Command`.
///
/// # Safety
/// This calls `pre_exec` which runs in the forked child between fork/exec.
/// Only async-signal-safe operations are used inside the hook.
pub fn apply_seccomp_pre_exec(
    cmd: &mut tokio::process::Command,
    profile_path: &Path,
) -> Result<(), AppError> {
    // Read and parse the profile at command-build time (in the parent).
    // The child only executes the pre-built BPF program.
    let filter = build_bpf_filter(profile_path)?;

    // SAFETY: pre_exec runs between fork() and exec() in the child process.
    // We only call prctl and seccomp syscalls which are async-signal-safe.
    unsafe {
        cmd.pre_exec(move || apply_filter(&filter));
    }

    Ok(())
}

/// A pre-built BPF filter program ready to be loaded into seccomp.
#[derive(Clone)]
#[allow(dead_code)]
struct BpfFilter {
    /// Raw BPF instructions as bytes
    program: Vec<u8>,
    /// Number of BPF instructions
    len: u16,
}

/// Parse the seccomp JSON profile and compile it into BPF instructions.
///
/// The JSON profile uses the Docker/OCI format with an allowlist:
/// ```json
/// { "defaultAction": "SCMP_ACT_ERRNO", "syscalls": [{ "names": [...], "action": "SCMP_ACT_ALLOW" }] }
/// ```
fn build_bpf_filter(profile_path: &Path) -> Result<BpfFilter, AppError> {
    let content = std::fs::read_to_string(profile_path).map_err(|e| {
        AppError::Internal(format!(
            "failed to read seccomp profile {}: {e}",
            profile_path.display()
        ))
    })?;

    let profile: serde_json::Value = serde_json::from_str(&content)
        .map_err(|e| AppError::Internal(format!("failed to parse seccomp profile: {e}")))?;

    // Extract allowed syscall names
    let allowed_syscalls = extract_allowed_syscalls(&profile)?;

    // Resolve syscall names to numbers for the current architecture
    let allowed_numbers = resolve_syscall_numbers(&allowed_syscalls);

    // Build a BPF program: allow listed syscalls, return EPERM for everything else
    compile_bpf_program(&allowed_numbers)
}

/// Extract syscall names with SCMP_ACT_ALLOW from the profile.
fn extract_allowed_syscalls(profile: &serde_json::Value) -> Result<Vec<String>, AppError> {
    let syscalls = profile["syscalls"]
        .as_array()
        .ok_or_else(|| AppError::Internal("seccomp profile missing 'syscalls' array".into()))?;

    let mut allowed = Vec::new();
    for entry in syscalls {
        let action = entry["action"].as_str().unwrap_or("");
        if action == "SCMP_ACT_ALLOW" {
            if let Some(names) = entry["names"].as_array() {
                for name in names {
                    if let Some(s) = name.as_str() {
                        allowed.push(s.to_owned());
                    }
                }
            }
        }
    }

    Ok(allowed)
}

/// Resolve syscall names to their numeric values on the current architecture.
/// Unknown syscalls are silently skipped (they may be for a different arch).
#[cfg(target_os = "linux")]
fn resolve_syscall_numbers(names: &[String]) -> Vec<u32> {
    let mut numbers = Vec::new();
    for name in names {
        if let Some(nr) = syscall_name_to_number(name) {
            numbers.push(nr);
        }
    }
    numbers.sort_unstable();
    numbers.dedup();
    numbers
}

#[cfg(not(target_os = "linux"))]
fn resolve_syscall_numbers(_names: &[String]) -> Vec<u32> {
    Vec::new()
}

/// Map a syscall name to its Linux number. This covers the common set used by
/// Deno/Node.js runtimes on x86_64 and aarch64.
#[cfg(target_os = "linux")]
fn syscall_name_to_number(name: &str) -> Option<u32> {
    // Use libc constants where available, with fallbacks for newer syscalls.
    // These are architecture-dependent; libc handles the mapping.
    let nr: i64 = match name {
        "read" => libc::SYS_read,
        "write" => libc::SYS_write,
        "close" => libc::SYS_close,
        #[cfg(target_arch = "x86_64")]
        "stat" => libc::SYS_stat,
        #[cfg(target_arch = "x86_64")]
        "fstat" => libc::SYS_fstat,
        #[cfg(target_arch = "x86_64")]
        "lstat" => libc::SYS_lstat,
        #[cfg(target_arch = "x86_64")]
        "poll" => libc::SYS_poll,
        "lseek" => libc::SYS_lseek,
        "mmap" => libc::SYS_mmap,
        "mprotect" => libc::SYS_mprotect,
        "munmap" => libc::SYS_munmap,
        "brk" => libc::SYS_brk,
        "rt_sigaction" => libc::SYS_rt_sigaction,
        "rt_sigprocmask" => libc::SYS_rt_sigprocmask,
        "rt_sigreturn" => libc::SYS_rt_sigreturn,
        "ioctl" => libc::SYS_ioctl,
        "pread64" => libc::SYS_pread64,
        "pwrite64" => libc::SYS_pwrite64,
        "readv" => libc::SYS_readv,
        "writev" => libc::SYS_writev,
        #[cfg(target_arch = "x86_64")]
        "access" => libc::SYS_access,
        #[cfg(target_arch = "x86_64")]
        "pipe" => libc::SYS_pipe,
        #[cfg(target_arch = "x86_64")]
        "select" => libc::SYS_select,
        "sched_yield" => libc::SYS_sched_yield,
        "mremap" => libc::SYS_mremap,
        "msync" => libc::SYS_msync,
        "mincore" => libc::SYS_mincore,
        "madvise" => libc::SYS_madvise,
        #[cfg(target_arch = "x86_64")]
        "dup" => libc::SYS_dup,
        #[cfg(target_arch = "x86_64")]
        "dup2" => libc::SYS_dup2,
        "dup3" => libc::SYS_dup3,
        "nanosleep" => libc::SYS_nanosleep,
        "clock_nanosleep" => libc::SYS_clock_nanosleep,
        "getpid" => libc::SYS_getpid,
        "getppid" => libc::SYS_getppid,
        "getuid" => libc::SYS_getuid,
        "getgid" => libc::SYS_getgid,
        "geteuid" => libc::SYS_geteuid,
        "getegid" => libc::SYS_getegid,
        "getgroups" => libc::SYS_getgroups,
        "gettid" => libc::SYS_gettid,
        "setsid" => libc::SYS_setsid,
        "socket" => libc::SYS_socket,
        "connect" => libc::SYS_connect,
        "accept" => libc::SYS_accept,
        "accept4" => libc::SYS_accept4,
        "sendto" => libc::SYS_sendto,
        "recvfrom" => libc::SYS_recvfrom,
        "sendmsg" => libc::SYS_sendmsg,
        "recvmsg" => libc::SYS_recvmsg,
        "bind" => libc::SYS_bind,
        "listen" => libc::SYS_listen,
        "getsockname" => libc::SYS_getsockname,
        "getpeername" => libc::SYS_getpeername,
        "setsockopt" => libc::SYS_setsockopt,
        "getsockopt" => libc::SYS_getsockopt,
        "shutdown" => libc::SYS_shutdown,
        "clone" => libc::SYS_clone,
        "clone3" => libc::SYS_clone3,
        "execve" => libc::SYS_execve,
        "exit" => libc::SYS_exit,
        "exit_group" => libc::SYS_exit_group,
        "wait4" => libc::SYS_wait4,
        "waitid" => libc::SYS_waitid,
        "kill" => libc::SYS_kill,
        "tgkill" => libc::SYS_tgkill,
        "uname" => libc::SYS_uname,
        "fcntl" => libc::SYS_fcntl,
        "flock" => libc::SYS_flock,
        "fsync" => libc::SYS_fsync,
        "fdatasync" => libc::SYS_fdatasync,
        "truncate" => libc::SYS_truncate,
        "ftruncate" => libc::SYS_ftruncate,
        "getdents64" => libc::SYS_getdents64,
        "getcwd" => libc::SYS_getcwd,
        "chdir" => libc::SYS_chdir,
        #[cfg(target_arch = "x86_64")]
        "rename" => libc::SYS_rename,
        "renameat" => libc::SYS_renameat,
        "renameat2" => libc::SYS_renameat2,
        #[cfg(target_arch = "x86_64")]
        "mkdir" => libc::SYS_mkdir,
        #[cfg(target_arch = "x86_64")]
        "rmdir" => libc::SYS_rmdir,
        #[cfg(target_arch = "x86_64")]
        "unlink" => libc::SYS_unlink,
        "unlinkat" => libc::SYS_unlinkat,
        #[cfg(target_arch = "x86_64")]
        "readlink" => libc::SYS_readlink,
        "readlinkat" => libc::SYS_readlinkat,
        #[cfg(target_arch = "x86_64")]
        "chmod" => libc::SYS_chmod,
        "fchmod" => libc::SYS_fchmod,
        #[cfg(target_arch = "x86_64")]
        "chown" => libc::SYS_chown,
        "fchown" => libc::SYS_fchown,
        "umask" => libc::SYS_umask,
        #[cfg(target_arch = "x86_64")]
        "gettimeofday" => libc::SYS_gettimeofday,
        "getrusage" => libc::SYS_getrusage,
        "sysinfo" => libc::SYS_sysinfo,
        "times" => libc::SYS_times,
        "getrlimit" => libc::SYS_getrlimit,
        "setrlimit" => libc::SYS_setrlimit,
        "prlimit64" => libc::SYS_prlimit64,
        "statfs" => libc::SYS_statfs,
        "fstatfs" => libc::SYS_fstatfs,
        "prctl" => libc::SYS_prctl,
        #[cfg(target_arch = "x86_64")]
        "arch_prctl" => libc::SYS_arch_prctl,
        "clock_gettime" => libc::SYS_clock_gettime,
        "clock_getres" => libc::SYS_clock_getres,
        #[cfg(target_arch = "x86_64")]
        "epoll_create" => libc::SYS_epoll_create,
        "epoll_create1" => libc::SYS_epoll_create1,
        "epoll_ctl" => libc::SYS_epoll_ctl,
        #[cfg(target_arch = "x86_64")]
        "epoll_wait" => libc::SYS_epoll_wait,
        "epoll_pwait" => libc::SYS_epoll_pwait,
        #[cfg(target_arch = "x86_64")]
        "eventfd" => libc::SYS_eventfd,
        "eventfd2" => libc::SYS_eventfd2,
        #[cfg(target_arch = "x86_64")]
        "signalfd" => libc::SYS_signalfd,
        "signalfd4" => libc::SYS_signalfd4,
        "timerfd_create" => libc::SYS_timerfd_create,
        "timerfd_settime" => libc::SYS_timerfd_settime,
        "timerfd_gettime" => libc::SYS_timerfd_gettime,
        "pipe2" => libc::SYS_pipe2,
        #[cfg(target_arch = "x86_64")]
        "inotify_init" => libc::SYS_inotify_init,
        "inotify_init1" => libc::SYS_inotify_init1,
        "inotify_add_watch" => libc::SYS_inotify_add_watch,
        "inotify_rm_watch" => libc::SYS_inotify_rm_watch,
        "openat" => libc::SYS_openat,
        "newfstatat" => libc::SYS_newfstatat,
        "futex" => libc::SYS_futex,
        "set_robust_list" => libc::SYS_set_robust_list,
        "get_robust_list" => libc::SYS_get_robust_list,
        "set_tid_address" => libc::SYS_set_tid_address,
        "memfd_create" => libc::SYS_memfd_create,
        "copy_file_range" => libc::SYS_copy_file_range,
        "statx" => libc::SYS_statx,
        "getrandom" => libc::SYS_getrandom,
        "rseq" => libc::SYS_rseq,
        "io_uring_setup" => libc::SYS_io_uring_setup,
        "io_uring_enter" => libc::SYS_io_uring_enter,
        "io_uring_register" => libc::SYS_io_uring_register,
        "faccessat" => libc::SYS_faccessat,
        "faccessat2" => libc::SYS_faccessat2,
        "pselect6" => libc::SYS_pselect6,
        "ppoll" => libc::SYS_ppoll,
        "sched_getaffinity" => libc::SYS_sched_getaffinity,
        "sched_setaffinity" => libc::SYS_sched_setaffinity,
        "mlock" => libc::SYS_mlock,
        "munlock" => libc::SYS_munlock,
        "mlock2" => libc::SYS_mlock2,
        "seccomp" => libc::SYS_seccomp,
        "close_range" => libc::SYS_close_range,
        "openat2" => libc::SYS_openat2,
        "pidfd_open" => libc::SYS_pidfd_open,
        "pidfd_send_signal" => libc::SYS_pidfd_send_signal,
        #[cfg(target_arch = "x86_64")]
        "getdents" => libc::SYS_getdents,
        _ => return None,
    };
    Some(nr as u32)
}

/// Compile a list of allowed syscall numbers into a BPF program.
///
/// The program structure:
/// 1. Load the syscall number (seccomp data offset 0)
/// 2. For each allowed syscall: if match, jump to ALLOW
/// 3. Default: return EPERM (errno 1)
/// 4. ALLOW: return ALLOW
#[cfg(target_os = "linux")]
fn compile_bpf_program(allowed: &[u32]) -> Result<BpfFilter, AppError> {
    // BPF instruction format: { code: u16, jt: u8, jf: u8, k: u32 }
    // Total size per instruction: 8 bytes

    // Constants for seccomp BPF
    const BPF_LD: u16 = 0x00;
    const BPF_W: u16 = 0x00;
    const BPF_ABS: u16 = 0x20;
    const BPF_JMP: u16 = 0x05;
    const BPF_JEQ: u16 = 0x10;
    const BPF_K: u16 = 0x00;
    const BPF_RET: u16 = 0x06;

    const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
    const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;

    // Offset of nr field in seccomp_data struct
    const SECCOMP_DATA_NR_OFFSET: u32 = 0;

    let n = allowed.len();
    // Instructions: 1 (load) + n (comparisons) + 1 (default deny) + 1 (allow) = n + 3
    let total_insns = n + 3;

    let mut program = Vec::with_capacity(total_insns * 8);

    // Helper to push a BPF instruction
    let push = |prog: &mut Vec<u8>, code: u16, jt: u8, jf: u8, k: u32| {
        prog.extend_from_slice(&code.to_ne_bytes());
        prog.push(jt);
        prog.push(jf);
        prog.extend_from_slice(&k.to_ne_bytes());
    };

    // Instruction 0: Load syscall number
    // BPF_LD | BPF_W | BPF_ABS, offset = 0 (nr field)
    push(
        &mut program,
        BPF_LD | BPF_W | BPF_ABS,
        0,
        0,
        SECCOMP_DATA_NR_OFFSET,
    );

    // Instructions 1..n: Compare against each allowed syscall
    // If match: jump to the ALLOW instruction (at offset n + 2 - current)
    // If no match: continue to next comparison
    for (i, &nr) in allowed.iter().enumerate() {
        let remaining = n - i; // number of comparisons left after this one (including default deny)
        let jt = remaining as u8; // jump over remaining comparisons + deny to reach ALLOW
        push(&mut program, BPF_JMP | BPF_JEQ | BPF_K, jt, 0, nr);
    }

    // Instruction n+1: Default deny — return EPERM
    push(&mut program, BPF_RET | BPF_K, 0, 0, SECCOMP_RET_ERRNO | 1);

    // Instruction n+2: Allow
    push(&mut program, BPF_RET | BPF_K, 0, 0, SECCOMP_RET_ALLOW);

    if total_insns > u16::MAX as usize {
        return Err(AppError::Internal("seccomp BPF program too large".into()));
    }

    Ok(BpfFilter {
        program,
        len: total_insns as u16,
    })
}

#[cfg(not(target_os = "linux"))]
fn compile_bpf_program(_allowed: &[u32]) -> Result<BpfFilter, AppError> {
    Ok(BpfFilter {
        program: Vec::new(),
        len: 0,
    })
}

/// Apply the BPF filter inside the child process (called from pre_exec).
#[cfg(target_os = "linux")]
fn apply_filter(filter: &BpfFilter) -> std::io::Result<()> {
    if filter.len == 0 {
        return Ok(());
    }

    // Step 1: PR_SET_NO_NEW_PRIVS — required before installing seccomp
    let ret = unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };
    if ret != 0 {
        return Err(std::io::Error::last_os_error());
    }

    // Step 2: Install the BPF filter via seccomp(2) syscall
    // struct sock_fprog { unsigned short len; struct sock_filter *filter; }
    #[repr(C)]
    struct SockFprog {
        len: u16,
        filter: *const u8,
    }

    let prog = SockFprog {
        len: filter.len,
        filter: filter.program.as_ptr(),
    };

    const SECCOMP_SET_MODE_FILTER: libc::c_ulong = 1;

    let ret = unsafe {
        libc::syscall(
            libc::SYS_seccomp,
            SECCOMP_SET_MODE_FILTER,
            0 as libc::c_ulong,
            &prog as *const SockFprog,
        )
    };

    if ret != 0 {
        return Err(std::io::Error::last_os_error());
    }

    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn apply_filter(_filter: &BpfFilter) -> std::io::Result<()> {
    // Seccomp is Linux-only; no-op on other platforms
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_allowed_from_profile() {
        let profile: serde_json::Value =
            serde_json::from_str(crate::runtime::pool::sandbox::SECCOMP_PROFILE).unwrap();
        let allowed = extract_allowed_syscalls(&profile).unwrap();
        assert!(allowed.contains(&"read".to_string()));
        assert!(allowed.contains(&"write".to_string()));
        assert!(allowed.contains(&"execve".to_string()));
        // Blocked syscalls should NOT be present
        assert!(!allowed.contains(&"ptrace".to_string()));
        assert!(!allowed.contains(&"mount".to_string()));
        assert!(!allowed.contains(&"bpf".to_string()));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn resolve_common_syscalls() {
        let names = vec![
            "read".into(),
            "write".into(),
            "close".into(),
            "nonexistent_syscall_xyz".into(),
        ];
        let numbers = resolve_syscall_numbers(&names);
        // Should resolve at least read, write, close
        assert!(numbers.len() >= 3);
        // nonexistent should be skipped
        assert!(numbers.len() <= 3);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn compile_program_structure() {
        let allowed = vec![0, 1, 2]; // read, write, close on x86_64
        let filter = compile_bpf_program(&allowed).unwrap();
        // n + 3 instructions, 8 bytes each
        assert_eq!(filter.len, 6); // 3 syscalls + load + deny + allow
        assert_eq!(filter.program.len(), 48); // 6 * 8
    }

    #[test]
    fn empty_filter_is_noop() {
        let filter = BpfFilter {
            program: Vec::new(),
            len: 0,
        };
        assert!(apply_filter(&filter).is_ok());
    }
}
