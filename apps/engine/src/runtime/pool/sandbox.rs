use std::path::{Path, PathBuf};

/// Seccomp BPF profile for worker processes.
///
/// This is a JSON seccomp profile compatible with Docker's `--security-opt seccomp=`
/// and can also be loaded via `seccomp_rule_add` at the Rust level.
///
/// Strategy: allowlist — only permit syscalls needed by Deno/Node.js.
/// Dangerous syscalls that could be used for sandbox escape are blocked.
pub const SECCOMP_PROFILE: &str = r#"{
  "defaultAction": "SCMP_ACT_ERRNO",
  "defaultErrnoRet": 1,
  "architectures": ["SCMP_ARCH_X86_64", "SCMP_ARCH_AARCH64"],
  "syscalls": [
    {
      "names": [
        "read", "write", "close", "fstat", "lstat", "stat",
        "poll", "lseek", "mmap", "mprotect", "munmap", "brk",
        "rt_sigaction", "rt_sigprocmask", "rt_sigreturn",
        "ioctl", "pread64", "pwrite64", "readv", "writev",
        "access", "pipe", "select", "sched_yield",
        "mremap", "msync", "mincore", "madvise",
        "dup", "dup2", "dup3", "nanosleep", "clock_nanosleep",
        "getpid", "getppid", "getuid", "getgid", "geteuid", "getegid",
        "getgroups", "gettid", "setsid",
        "socket", "connect", "accept", "accept4",
        "sendto", "recvfrom", "sendmsg", "recvmsg",
        "bind", "listen", "getsockname", "getpeername",
        "setsockopt", "getsockopt", "shutdown",
        "clone", "clone3", "execve", "exit", "exit_group",
        "wait4", "waitid", "kill", "tgkill",
        "uname", "fcntl", "flock", "fsync", "fdatasync",
        "truncate", "ftruncate", "getdents", "getdents64",
        "getcwd", "chdir", "rename", "renameat", "renameat2",
        "mkdir", "rmdir", "unlink", "unlinkat", "readlink", "readlinkat",
        "chmod", "fchmod", "chown", "fchown",
        "umask", "gettimeofday", "getrusage", "sysinfo",
        "times", "getrlimit", "setrlimit", "prlimit64",
        "statfs", "fstatfs",
        "prctl", "arch_prctl",
        "clock_gettime", "clock_getres",
        "epoll_create", "epoll_create1", "epoll_ctl", "epoll_wait", "epoll_pwait",
        "eventfd", "eventfd2", "signalfd", "signalfd4",
        "timerfd_create", "timerfd_settime", "timerfd_gettime",
        "pipe2", "inotify_init", "inotify_init1", "inotify_add_watch", "inotify_rm_watch",
        "openat", "newfstatat", "futex",
        "set_robust_list", "get_robust_list",
        "set_tid_address",
        "memfd_create", "copy_file_range",
        "statx", "getrandom",
        "rseq", "io_uring_setup", "io_uring_enter", "io_uring_register",
        "faccessat", "faccessat2",
        "pselect6", "ppoll",
        "sched_getaffinity", "sched_setaffinity",
        "mlock", "munlock", "mlock2",
        "seccomp",
        "close_range", "openat2",
        "pidfd_open", "pidfd_send_signal"
      ],
      "action": "SCMP_ACT_ALLOW"
    }
  ]
}"#;

/// Blocked syscalls and the reason they're blocked:
///
/// - `ptrace` — Process debugging/inspection, could inspect other workers
/// - `mount` / `umount2` — Filesystem manipulation
/// - `init_module` / `finit_module` — Kernel module loading
/// - `kexec_load` — Load a new kernel
/// - `bpf` — eBPF program loading (powerful kernel interface)
/// - `perf_event_open` — Performance monitoring (timing side channels)
/// - `acct` — Process accounting
/// - `quotactl` — Disk quota control
/// - `swapon` / `swapoff` — Swap control
/// - `reboot` — System reboot
/// - `sethostname` / `setdomainname` — Hostname modification
/// - `settimeofday` / `clock_settime` — Time manipulation
/// - `add_key` / `keyctl` / `request_key` — Kernel keyring access
/// - `pivot_root` / `chroot` — Root filesystem changes
/// - `personality` — Change execution domain (could disable ASLR)
/// - `userfaultfd` — User-space page fault handling (potential exploit vector)
///
/// Seccomp enforcement state for the worker pool.
#[derive(Debug, Clone)]
pub struct SeccompEnforcer {
    /// Whether seccomp enforcement is required. When true, workers that fail
    /// seccomp setup will not be started.
    pub enforce: bool,
    /// Path to the written seccomp profile file, if available.
    pub profile_path: Option<PathBuf>,
}

impl SeccompEnforcer {
    /// Initialize seccomp enforcement. Writes the profile to `dir` and checks availability.
    ///
    /// If `enforce` is true and seccomp is not available, returns an error
    /// (fail-safe: do not start insecurely in production).
    pub fn init(dir: &Path, enforce: bool) -> Result<Self, crate::error::AppError> {
        let available = is_seccomp_available();

        if enforce && !available {
            return Err(crate::error::AppError::Internal(
                "seccomp enforcement is enabled (RIFT_SECCOMP_ENFORCE=true) but seccomp \
                 is not available on this system. Refusing to start insecurely. \
                 Set RIFT_SECCOMP_ENFORCE=false to disable for development."
                    .to_string(),
            ));
        }

        let profile_path = if available {
            match write_seccomp_profile(dir) {
                Ok(path) => {
                    tracing::info!(
                        path = %path.display(),
                        enforce,
                        "seccomp profile written"
                    );
                    Some(path)
                }
                Err(e) => {
                    if enforce {
                        return Err(crate::error::AppError::Internal(format!(
                            "seccomp enforcement enabled but failed to write profile: {e}"
                        )));
                    }
                    tracing::warn!(error = %e, "failed to write seccomp profile, continuing without enforcement");
                    None
                }
            }
        } else {
            tracing::info!("seccomp not available on this platform, skipping profile write");
            None
        };

        Ok(Self {
            enforce,
            profile_path,
        })
    }

    /// Return Docker-compatible `--security-opt` argument for spawning contained processes,
    /// or None if seccomp is not available / not enforced.
    pub fn docker_security_opt(&self) -> Option<String> {
        self.profile_path
            .as_ref()
            .map(|p| format!("seccomp={}", p.display()))
    }

    /// Check if seccomp should be applied to worker processes.
    pub fn should_apply(&self) -> bool {
        self.profile_path.is_some()
    }
}

/// Write the seccomp profile to a file for use with process spawning.
pub fn write_seccomp_profile(dir: &Path) -> Result<PathBuf, crate::error::AppError> {
    let profile_path = dir.join("rift-worker-seccomp.json");
    std::fs::write(&profile_path, SECCOMP_PROFILE).map_err(|e| {
        crate::error::AppError::Internal(format!("failed to write seccomp profile: {e}"))
    })?;
    Ok(profile_path)
}

/// Check if seccomp is available on this system.
pub fn is_seccomp_available() -> bool {
    // Check if /proc/self/status contains Seccomp
    std::fs::read_to_string("/proc/self/status")
        .map(|s| s.contains("Seccomp:"))
        .unwrap_or(false)
}
