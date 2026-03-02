//! Tenant resource governance: policy model and enforcement.
//!
//! Defines per-project runtime resource limits with global defaults and
//! optional per-project overrides. Separates build-time and runtime policies.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::config::Config;
use crate::error::AppError;

use super::pool::limits::ResourceLimits;

/// Runtime resource policy applied to a running project.
///
/// Each field governs a specific resource dimension. Values come from
/// global config defaults, optionally overridden per-project.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RuntimePolicy {
    /// Maximum memory in bytes (hard OOM kill limit).
    pub memory_max_bytes: u64,
    /// Memory high watermark in bytes — triggers throttling before OOM.
    pub memory_high_bytes: u64,
    /// CPU quota in microseconds per 100ms period (100_000 = 100% of one core).
    pub cpu_quota_us: u64,
    /// CPU period in microseconds (fixed at 100ms).
    pub cpu_period_us: u64,
    /// Maximum number of PIDs (prevents fork bombs).
    pub max_pids: u32,
    /// Maximum number of open file descriptors.
    pub max_open_files: u32,
    /// Per-request timeout budget in seconds.
    pub request_timeout_secs: u64,
    /// Maximum concurrent in-flight requests per project.
    pub max_concurrent_requests: u32,
}

impl Default for RuntimePolicy {
    fn default() -> Self {
        Self {
            memory_max_bytes: 512 * 1024 * 1024,  // 512 MB
            memory_high_bytes: 384 * 1024 * 1024, // 384 MB
            cpu_quota_us: 100_000,                // 100% of one core
            cpu_period_us: 100_000,               // 100ms period
            max_pids: 64,
            max_open_files: 1024,
            request_timeout_secs: 30,
            max_concurrent_requests: 100,
        }
    }
}

impl RuntimePolicy {
    /// Convert to cgroup-level `ResourceLimits` for enforcement.
    pub fn to_resource_limits(&self) -> ResourceLimits {
        ResourceLimits {
            memory_max: self.memory_max_bytes,
            memory_high: self.memory_high_bytes,
            cpu_quota_us: self.cpu_quota_us,
            cpu_period_us: self.cpu_period_us,
            max_pids: self.max_pids,
        }
    }
}

/// Build-time resource policy applied during the build phase.
///
/// Deliberately separate from runtime policy so builds cannot inherit
/// or accidentally use runtime constraints.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BuildPolicy {
    /// Maximum memory in bytes for the build process.
    pub memory_max_bytes: u64,
    /// CPU quota in microseconds per 100ms period.
    pub cpu_quota_us: u64,
    /// CPU period in microseconds.
    pub cpu_period_us: u64,
    /// Maximum number of PIDs.
    pub max_pids: u32,
    /// Build timeout in seconds.
    pub build_timeout_secs: u64,
}

impl Default for BuildPolicy {
    fn default() -> Self {
        Self {
            memory_max_bytes: 2 * 1024 * 1024 * 1024, // 2 GB (builds are memory-hungry)
            cpu_quota_us: 200_000,                    // 200% = 2 cores
            cpu_period_us: 100_000,
            max_pids: 256,           // npm/pnpm fork heavily
            build_timeout_secs: 600, // 10 minutes
        }
    }
}

impl BuildPolicy {
    /// Convert to cgroup-level `ResourceLimits` for enforcement.
    pub fn to_resource_limits(&self) -> ResourceLimits {
        ResourceLimits {
            memory_max: self.memory_max_bytes,
            memory_high: self.memory_max_bytes * 3 / 4, // 75% as high watermark
            cpu_quota_us: self.cpu_quota_us,
            cpu_period_us: self.cpu_period_us,
            max_pids: self.max_pids,
        }
    }
}

/// Per-project policy overrides. `None` fields inherit from global defaults.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectPolicyOverrides {
    pub memory_max_bytes: Option<u64>,
    pub memory_high_bytes: Option<u64>,
    pub cpu_quota_us: Option<u64>,
    pub max_pids: Option<u32>,
    pub max_open_files: Option<u32>,
    pub request_timeout_secs: Option<u64>,
    pub max_concurrent_requests: Option<u32>,
    pub build_memory_max_bytes: Option<u64>,
    pub build_cpu_quota_us: Option<u64>,
    pub build_timeout_secs: Option<u64>,
}

/// Enforcement mode for resource limits.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EnforcementMode {
    /// Hard enforcement: fail deployment if limits cannot be applied.
    Strict,
    /// Best-effort: warn and continue if limits cannot be applied.
    #[default]
    BestEffort,
}

impl EnforcementMode {
    /// Parse enforcement mode from config string.
    pub fn from_config(config: &Config) -> Self {
        match config.resource_enforcement.as_str() {
            "strict" => Self::Strict,
            _ => Self::BestEffort,
        }
    }
}

/// Error taxonomy for resource enforcement failures.
#[derive(Debug, Clone, PartialEq)]
pub enum ResourceError {
    /// cgroup v2 is not available on this system.
    CgroupUnavailable,
    /// Failed to create or write cgroup for a worker.
    CgroupSetupFailed { worker_id: Uuid, detail: String },
    /// Failed to add a process to its cgroup.
    CgroupAttachFailed {
        worker_id: Uuid,
        pid: u32,
        detail: String,
    },
    /// Failed to tear down a worker's cgroup.
    CgroupTeardownFailed { worker_id: Uuid, detail: String },
    /// Project has exceeded its concurrent request limit.
    ConcurrentRequestLimitExceeded { project_id: Uuid, limit: u32 },
    /// Pool is at capacity and cannot accept new deployments.
    PoolCapacityExceeded { active: usize, max: usize },
}

impl std::fmt::Display for ResourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CgroupUnavailable => {
                write!(f, "cgroup v2 not available on this system")
            }
            Self::CgroupSetupFailed { worker_id, detail } => {
                write!(f, "cgroup setup failed for worker {worker_id}: {detail}")
            }
            Self::CgroupAttachFailed {
                worker_id,
                pid,
                detail,
            } => {
                write!(
                    f,
                    "failed to attach pid {pid} to cgroup for worker {worker_id}: {detail}"
                )
            }
            Self::CgroupTeardownFailed { worker_id, detail } => {
                write!(f, "cgroup teardown failed for worker {worker_id}: {detail}")
            }
            Self::ConcurrentRequestLimitExceeded { project_id, limit } => {
                write!(
                    f,
                    "project {project_id} exceeded concurrent request limit ({limit})"
                )
            }
            Self::PoolCapacityExceeded { active, max } => {
                write!(f, "pool at capacity ({active}/{max} active workers)")
            }
        }
    }
}

impl From<ResourceError> for AppError {
    fn from(e: ResourceError) -> Self {
        match &e {
            ResourceError::ConcurrentRequestLimitExceeded { .. } => {
                AppError::RateLimited(e.to_string())
            }
            ResourceError::PoolCapacityExceeded { .. } => AppError::Conflict(e.to_string()),
            _ => AppError::Internal(e.to_string()),
        }
    }
}

/// Resolve the effective runtime policy for a given project.
///
/// Starts from global config defaults, then applies per-project overrides.
pub fn resolve_runtime_policy(
    config: &Config,
    overrides: Option<&ProjectPolicyOverrides>,
) -> RuntimePolicy {
    let base = RuntimePolicy {
        memory_max_bytes: config.worker_memory_limit_mb * 1024 * 1024,
        memory_high_bytes: config.worker_memory_limit_mb * 1024 * 1024 * 3 / 4,
        cpu_quota_us: config.worker_cpu_quota_us,
        cpu_period_us: 100_000,
        max_pids: config.worker_max_pids,
        max_open_files: config.worker_max_open_files,
        request_timeout_secs: config.worker_request_timeout_secs,
        max_concurrent_requests: config.worker_max_concurrent_requests,
    };

    match overrides {
        None => base,
        Some(ov) => RuntimePolicy {
            memory_max_bytes: ov.memory_max_bytes.unwrap_or(base.memory_max_bytes),
            memory_high_bytes: ov.memory_high_bytes.unwrap_or(base.memory_high_bytes),
            cpu_quota_us: ov.cpu_quota_us.unwrap_or(base.cpu_quota_us),
            cpu_period_us: base.cpu_period_us,
            max_pids: ov.max_pids.unwrap_or(base.max_pids),
            max_open_files: ov.max_open_files.unwrap_or(base.max_open_files),
            request_timeout_secs: ov.request_timeout_secs.unwrap_or(base.request_timeout_secs),
            max_concurrent_requests: ov
                .max_concurrent_requests
                .unwrap_or(base.max_concurrent_requests),
        },
    }
}

/// Resolve the effective build policy for a given project.
pub fn resolve_build_policy(
    config: &Config,
    overrides: Option<&ProjectPolicyOverrides>,
) -> BuildPolicy {
    let base = BuildPolicy {
        memory_max_bytes: config.build_memory_limit_mb * 1024 * 1024,
        cpu_quota_us: config.build_cpu_quota_us,
        cpu_period_us: 100_000,
        max_pids: config.build_max_pids,
        build_timeout_secs: config.build_timeout_secs,
    };

    match overrides {
        None => base,
        Some(ov) => BuildPolicy {
            memory_max_bytes: ov.build_memory_max_bytes.unwrap_or(base.memory_max_bytes),
            cpu_quota_us: ov.build_cpu_quota_us.unwrap_or(base.cpu_quota_us),
            cpu_period_us: base.cpu_period_us,
            max_pids: base.max_pids,
            build_timeout_secs: ov.build_timeout_secs.unwrap_or(base.build_timeout_secs),
        },
    }
}

/// Apply cgroup resource limits for a worker with enforcement mode awareness.
///
/// In `Strict` mode, any failure to set up cgroups returns an error.
/// In `BestEffort` mode, failures are logged and the worker proceeds without limits.
pub fn enforce_cgroup_limits(
    worker_id: &Uuid,
    pid: u32,
    limits: &ResourceLimits,
    mode: EnforcementMode,
) -> Result<(), ResourceError> {
    use super::pool::limits;

    if !limits::is_cgroup_v2_available() {
        if mode == EnforcementMode::Strict {
            crate::metrics::RESOURCE_VIOLATION
                .with_label_values(&["cgroup_unavailable"])
                .inc();
            return Err(ResourceError::CgroupUnavailable);
        }
        tracing::warn!("cgroup v2 not available, resource limits will not be enforced");
        return Ok(());
    }

    if let Err(e) = limits::setup_cgroup(worker_id, limits) {
        let err = ResourceError::CgroupSetupFailed {
            worker_id: *worker_id,
            detail: e.to_string(),
        };
        crate::metrics::RESOURCE_VIOLATION
            .with_label_values(&["cgroup_setup_failed"])
            .inc();
        if mode == EnforcementMode::Strict {
            return Err(err);
        }
        tracing::warn!(error = %err, "cgroup setup failed, continuing without limits");
        return Ok(());
    }

    if let Err(e) = limits::add_process_to_cgroup(worker_id, pid) {
        let err = ResourceError::CgroupAttachFailed {
            worker_id: *worker_id,
            pid,
            detail: e.to_string(),
        };
        crate::metrics::RESOURCE_VIOLATION
            .with_label_values(&["cgroup_attach_failed"])
            .inc();
        if mode == EnforcementMode::Strict {
            // Clean up the cgroup we just created
            let _ = limits::teardown_cgroup(worker_id);
            return Err(err);
        }
        tracing::warn!(error = %err, "failed to attach process to cgroup, continuing without limits");
    }

    Ok(())
}

/// Tear down cgroup resources for a worker, logging failures.
pub fn release_cgroup(worker_id: &Uuid) {
    use super::pool::limits;

    if let Err(e) = limits::teardown_cgroup(worker_id) {
        tracing::warn!(
            worker_id = %worker_id,
            error = %e,
            "cgroup teardown failed"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> Config {
        Config {
            database_url: String::new(),
            master_key: String::new(),
            jwt_private_key_pem: String::new(),
            jwt_public_key_pem: String::new(),
            internal_api_token: String::new(),
            api_bind: "0.0.0.0".into(),
            api_port: 3001,
            proxy_bind: "0.0.0.0".into(),
            proxy_port: 8080,
            proxy_upstream_timeout_ms: 30_000,
            proxy_connect_timeout_ms: 3_000,
            proxy_pool_max_idle_per_host: 32,
            proxy_max_inflight: 2_000,
            public_port: None,
            base_domain: "localhost".into(),
            proxy_scheme: "http".into(),
            access_token_ttl_minutes: 15,
            refresh_token_ttl_days: 7,
            cookie_secure: false,
            cors_origin: None,
            build_root: "/tmp/builds".into(),
            deploy_root: "/tmp/deploys".into(),
            public_ip: None,
            ssl_dir: "/tmp/ssl".into(),
            acme_email: None,
            acme_staging: false,
            https_port: 8443,
            state_store: "local".into(),
            redis_url: "redis://127.0.0.1:6379".into(),
            abuse_allowlist_cidrs: String::new(),
            abuse_bypass_token: None,
            abuse_bypass_header: "x-rift-abuse-bypass".into(),
            abuse_limit_overrides_json: None,
            abuse_challenge_ttl_secs: 900,
            abuse_bot_verify: false,
            abuse_bot_verify_cache_secs: 600,
            abuse_challenge_min_solve_secs: 2,
            abuse_max_retry_after_secs: 600,
            abuse_ban_tier1_secs: 60,
            abuse_ban_tier2_secs: 300,
            abuse_ban_tier3_secs: 1800,
            abuse_turnstile_site_key: None,
            abuse_turnstile_secret_key: None,
            worker_id: None,
            role: "control-plane".into(),
            region_id: "test".into(),
            node_id: Some("test-node".into()),
            jetstream_url: "nats://127.0.0.1:4222".into(),
            artifact_store_url: None,
            artifact_store_bucket: "rift-artifacts".into(),
            artifact_signing_private_key: None,
            artifact_signing_public_key: None,
            route_propagation_sla_ms: 2000,
            edge_heartbeat_interval_ms: 5000,
            runtime_mode: "process".into(),
            pool_warm_size: 3,
            pool_max_active: 50,
            worker_memory_limit_mb: 512,
            worker_loader: "/tmp/loader.ts".into(),
            global_dispatcher_port: 9999,
            function_mode: "isolate".into(),
            isolate_max_concurrent: 50,
            isolate_timeout_secs: 30,
            isolate_heap_limit_mb: 128,
            seccomp_enforce: false,
            namespace_isolate: false,
            build_concurrency: 4,
            build_cache_dir: "/tmp/cache".into(),
            build_clean_cache: false,
            install_skip_on_cache_hit: true,
            artifact_copy_mode: "auto".into(),
            healthcheck_interval_ms: 200,
            healthcheck_attempts: 50,
            // Phase 5 fields
            worker_cpu_quota_us: 100_000,
            worker_max_pids: 64,
            worker_max_open_files: 1024,
            worker_request_timeout_secs: 30,
            worker_max_concurrent_requests: 100,
            resource_enforcement: "best-effort".into(),
            build_memory_limit_mb: 2048,
            build_cpu_quota_us: 200_000,
            build_max_pids: 256,
            build_timeout_secs: 600,
        }
    }

    #[test]
    fn default_runtime_policy_uses_config_defaults() {
        let config = test_config();
        let policy = resolve_runtime_policy(&config, None);

        assert_eq!(policy.memory_max_bytes, 512 * 1024 * 1024);
        assert_eq!(policy.cpu_quota_us, 100_000);
        assert_eq!(policy.max_pids, 64);
        assert_eq!(policy.max_open_files, 1024);
        assert_eq!(policy.request_timeout_secs, 30);
        assert_eq!(policy.max_concurrent_requests, 100);
    }

    #[test]
    fn project_overrides_apply_selectively() {
        let config = test_config();
        let overrides = ProjectPolicyOverrides {
            memory_max_bytes: Some(256 * 1024 * 1024),
            cpu_quota_us: Some(50_000),
            max_concurrent_requests: Some(10),
            ..Default::default()
        };

        let policy = resolve_runtime_policy(&config, Some(&overrides));

        assert_eq!(policy.memory_max_bytes, 256 * 1024 * 1024);
        assert_eq!(policy.cpu_quota_us, 50_000);
        assert_eq!(policy.max_concurrent_requests, 10);
        // Non-overridden fields use defaults
        assert_eq!(policy.max_pids, 64);
        assert_eq!(policy.max_open_files, 1024);
        assert_eq!(policy.request_timeout_secs, 30);
    }

    #[test]
    fn runtime_policy_converts_to_resource_limits() {
        let policy = RuntimePolicy::default();
        let limits = policy.to_resource_limits();

        assert_eq!(limits.memory_max, policy.memory_max_bytes);
        assert_eq!(limits.memory_high, policy.memory_high_bytes);
        assert_eq!(limits.cpu_quota_us, policy.cpu_quota_us);
        assert_eq!(limits.cpu_period_us, policy.cpu_period_us);
        assert_eq!(limits.max_pids, policy.max_pids);
    }

    #[test]
    fn build_policy_defaults_are_distinct_from_runtime() {
        let config = test_config();
        let runtime = resolve_runtime_policy(&config, None);
        let build = resolve_build_policy(&config, None);

        // Builds get more resources than runtime
        assert!(build.memory_max_bytes > runtime.memory_max_bytes);
        assert!(build.cpu_quota_us > runtime.cpu_quota_us);
        assert!(build.max_pids > runtime.max_pids);
    }

    #[test]
    fn build_overrides_do_not_affect_runtime() {
        let config = test_config();
        let overrides = ProjectPolicyOverrides {
            build_memory_max_bytes: Some(4 * 1024 * 1024 * 1024),
            build_timeout_secs: Some(1200),
            ..Default::default()
        };

        let runtime = resolve_runtime_policy(&config, Some(&overrides));
        let build = resolve_build_policy(&config, Some(&overrides));

        assert_eq!(runtime.memory_max_bytes, 512 * 1024 * 1024);
        assert_eq!(build.memory_max_bytes, 4 * 1024 * 1024 * 1024);
        assert_eq!(build.build_timeout_secs, 1200);
    }

    #[test]
    fn enforcement_mode_default_is_best_effort() {
        assert_eq!(EnforcementMode::default(), EnforcementMode::BestEffort);
    }

    #[test]
    fn resource_error_display() {
        let err = ResourceError::CgroupUnavailable;
        assert_eq!(err.to_string(), "cgroup v2 not available on this system");

        let err = ResourceError::PoolCapacityExceeded {
            active: 50,
            max: 50,
        };
        assert_eq!(err.to_string(), "pool at capacity (50/50 active workers)");
    }

    #[test]
    fn resource_error_converts_to_app_error() {
        let pid = Uuid::new_v4();

        let err: AppError = ResourceError::ConcurrentRequestLimitExceeded {
            project_id: pid,
            limit: 100,
        }
        .into();
        assert!(matches!(err, AppError::RateLimited(_)));

        let err: AppError = ResourceError::PoolCapacityExceeded {
            active: 50,
            max: 50,
        }
        .into();
        assert!(matches!(err, AppError::Conflict(_)));

        let err: AppError = ResourceError::CgroupUnavailable.into();
        assert!(matches!(err, AppError::Internal(_)));
    }

    #[test]
    fn enforcement_mode_from_config() {
        let mut config = test_config();

        config.resource_enforcement = "strict".into();
        assert_eq!(
            EnforcementMode::from_config(&config),
            EnforcementMode::Strict
        );

        config.resource_enforcement = "best-effort".into();
        assert_eq!(
            EnforcementMode::from_config(&config),
            EnforcementMode::BestEffort
        );

        config.resource_enforcement = "anything-else".into();
        assert_eq!(
            EnforcementMode::from_config(&config),
            EnforcementMode::BestEffort
        );
    }

    #[test]
    fn build_policy_converts_to_resource_limits() {
        let policy = BuildPolicy::default();
        let limits = policy.to_resource_limits();

        assert_eq!(limits.memory_max, policy.memory_max_bytes);
        assert_eq!(limits.memory_high, policy.memory_max_bytes * 3 / 4);
        assert_eq!(limits.cpu_quota_us, policy.cpu_quota_us);
        assert_eq!(limits.max_pids, policy.max_pids);
    }
}
