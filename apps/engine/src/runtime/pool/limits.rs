use std::path::Path;

use uuid::Uuid;

use crate::error::AppError;

/// Resource limits applied to each worker process via cgroups v2.
#[derive(Debug, Clone)]
pub struct ResourceLimits {
    /// Maximum memory in bytes (hard limit).
    pub memory_max: u64,
    /// Memory high watermark — triggers throttling before OOM.
    pub memory_high: u64,
    /// CPU quota in microseconds per 100ms period.
    /// 100_000 = 100% of one core, 50_000 = 50%.
    pub cpu_quota_us: u64,
    /// CPU period in microseconds (default 100ms).
    pub cpu_period_us: u64,
    /// Maximum number of PIDs (prevents fork bombs).
    pub max_pids: u32,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            memory_max: 512 * 1024 * 1024,     // 512 MB
            memory_high: 384 * 1024 * 1024,     // 384 MB (throttle before OOM)
            cpu_quota_us: 100_000,               // 100% of one core
            cpu_period_us: 100_000,              // 100ms period
            max_pids: 64,
        }
    }
}

const CGROUP_BASE: &str = "/sys/fs/cgroup/rift/workers";

/// Create a cgroup for a worker and apply resource limits.
///
/// Requires cgroup v2 unified hierarchy and write access to the cgroup fs.
/// In Docker, mount the cgroup filesystem and grant SYS_ADMIN capability.
pub fn setup_cgroup(
    worker_id: &Uuid,
    limits: &ResourceLimits,
) -> Result<(), AppError> {
    let cgroup_path = format!("{CGROUP_BASE}/{worker_id}");

    // Create cgroup directory
    std::fs::create_dir_all(&cgroup_path).map_err(|e| {
        AppError::Internal(format!(
            "failed to create cgroup at {cgroup_path}: {e}"
        ))
    })?;

    // Memory limits
    write_cgroup_file(&cgroup_path, "memory.max", &limits.memory_max.to_string())?;
    write_cgroup_file(
        &cgroup_path,
        "memory.high",
        &limits.memory_high.to_string(),
    )?;

    // CPU limits
    write_cgroup_file(
        &cgroup_path,
        "cpu.max",
        &format!("{} {}", limits.cpu_quota_us, limits.cpu_period_us),
    )?;

    // PID limits
    write_cgroup_file(&cgroup_path, "pids.max", &limits.max_pids.to_string())?;

    Ok(())
}

/// Add a process to a worker's cgroup.
pub fn add_process_to_cgroup(worker_id: &Uuid, pid: u32) -> Result<(), AppError> {
    let cgroup_path = format!("{CGROUP_BASE}/{worker_id}");
    write_cgroup_file(&cgroup_path, "cgroup.procs", &pid.to_string())
}

/// Clean up a worker's cgroup.
pub fn teardown_cgroup(worker_id: &Uuid) -> Result<(), AppError> {
    let cgroup_path = format!("{CGROUP_BASE}/{worker_id}");

    // Move processes to parent first
    let procs_path = format!("{cgroup_path}/cgroup.procs");
    if let Ok(procs) = std::fs::read_to_string(&procs_path) {
        let parent_procs = format!("{CGROUP_BASE}/cgroup.procs");
        for pid in procs.lines() {
            let _ = std::fs::write(&parent_procs, pid);
        }
    }

    // Remove cgroup directory
    if Path::new(&cgroup_path).exists() {
        std::fs::remove_dir(&cgroup_path).map_err(|e| {
            AppError::Internal(format!(
                "failed to remove cgroup at {cgroup_path}: {e}"
            ))
        })?;
    }

    Ok(())
}

/// Read current memory usage of a worker's cgroup.
pub fn read_memory_usage(worker_id: &Uuid) -> Option<u64> {
    let path = format!("{CGROUP_BASE}/{worker_id}/memory.current");
    std::fs::read_to_string(path)
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// Check if cgroup v2 is available on this system.
pub fn is_cgroup_v2_available() -> bool {
    Path::new("/sys/fs/cgroup/cgroup.controllers").exists()
}

/// Ensure the base cgroup directory for rift workers exists.
pub fn ensure_base_cgroup() -> Result<(), AppError> {
    if !is_cgroup_v2_available() {
        tracing::warn!("cgroup v2 not available, resource limits will not be enforced");
        return Ok(());
    }

    std::fs::create_dir_all(CGROUP_BASE).map_err(|e| {
        AppError::Internal(format!(
            "failed to create base cgroup at {CGROUP_BASE}: {e}"
        ))
    })?;

    // Enable controllers for child cgroups
    let controllers_path = format!("{CGROUP_BASE}/../cgroup.subtree_control");
    if Path::new(&controllers_path).exists() {
        let _ = std::fs::write(&controllers_path, "+memory +cpu +pids");
    }

    Ok(())
}

fn write_cgroup_file(cgroup_path: &str, file: &str, value: &str) -> Result<(), AppError> {
    let path = format!("{cgroup_path}/{file}");
    std::fs::write(&path, value).map_err(|e| {
        AppError::Internal(format!("failed to write {path}: {e}"))
    })
}
