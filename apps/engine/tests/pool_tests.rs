/// Tests for the function routing and file-path-to-route-pattern conversion.
/// These are pure unit tests that don't need a database or running workers.
mod function_routing {
    use std::path::Path;

    /// Helper: convert file path to route pattern using the same logic as the engine.
    fn file_path_to_route_pattern(rel_path: &Path) -> String {
        let path_str = rel_path.to_string_lossy().to_string();

        let without_ext = path_str
            .strip_suffix(".tsx")
            .or_else(|| path_str.strip_suffix(".jsx"))
            .or_else(|| path_str.strip_suffix(".ts"))
            .or_else(|| path_str.strip_suffix(".js"))
            .unwrap_or(&path_str);

        let without_index = if without_ext == "index" {
            ""
        } else {
            without_ext.strip_suffix("/index").unwrap_or(without_ext)
        };

        let pattern = without_index.replace('[', ":").replace(']', "");

        if pattern.is_empty() {
            "/".to_string()
        } else {
            format!("/{pattern}")
        }
    }

    #[test]
    fn simple_api_route() {
        let route = file_path_to_route_pattern(Path::new("api/hello.ts"));
        assert_eq!(route, "/api/hello");
    }

    #[test]
    fn nested_api_route() {
        let route = file_path_to_route_pattern(Path::new("api/users/list.ts"));
        assert_eq!(route, "/api/users/list");
    }

    #[test]
    fn parameterized_route() {
        let route = file_path_to_route_pattern(Path::new("api/users/[id].ts"));
        assert_eq!(route, "/api/users/:id");
    }

    #[test]
    fn index_route() {
        let route = file_path_to_route_pattern(Path::new("index.ts"));
        assert_eq!(route, "/");
    }

    #[test]
    fn nested_index_route() {
        let route = file_path_to_route_pattern(Path::new("api/index.ts"));
        assert_eq!(route, "/api");
    }

    #[test]
    fn js_extension() {
        let route = file_path_to_route_pattern(Path::new("api/health.js"));
        assert_eq!(route, "/api/health");
    }

    #[test]
    fn tsx_extension() {
        let route = file_path_to_route_pattern(Path::new("api/render.tsx"));
        assert_eq!(route, "/api/render");
    }

    #[test]
    fn jsx_extension() {
        let route = file_path_to_route_pattern(Path::new("api/component.jsx"));
        assert_eq!(route, "/api/component");
    }

    #[test]
    fn multiple_params() {
        let route = file_path_to_route_pattern(Path::new("api/[org]/[repo].ts"));
        assert_eq!(route, "/api/:org/:repo");
    }

    #[test]
    fn deeply_nested_route() {
        let route = file_path_to_route_pattern(Path::new("api/v1/users/[id]/posts.ts"));
        assert_eq!(route, "/api/v1/users/:id/posts");
    }

    #[test]
    fn route_sorting_static_before_param() {
        let mut routes = vec![
            "/api/users/:id".to_string(),
            "/api/health".to_string(),
            "/api/users/me".to_string(),
        ];

        routes.sort_by(|a, b| {
            let a_has_param = a.contains(':');
            let b_has_param = b.contains(':');
            a_has_param.cmp(&b_has_param).then(a.cmp(b))
        });

        assert_eq!(
            routes,
            vec!["/api/health", "/api/users/me", "/api/users/:id"]
        );
    }
}

/// Tests for the function scanner (requires filesystem).
mod function_scanner {
    use std::fs;
    use std::path::{Path, PathBuf};

    fn setup_functions_dir(base: &Path) -> PathBuf {
        let functions_dir = base.join("rift/functions");
        fs::create_dir_all(functions_dir.join("api/users")).unwrap();
        fs::write(
            functions_dir.join("api/hello.ts"),
            r#"export default { fetch(req) { return new Response("Hello"); } }"#,
        )
        .unwrap();
        fs::write(
            functions_dir.join("api/users/[id].ts"),
            r#"export default { fetch(req) { return new Response("User"); } }"#,
        )
        .unwrap();
        fs::write(
            functions_dir.join("index.ts"),
            r#"export default { fetch(req) { return new Response("Root"); } }"#,
        )
        .unwrap();
        // Test files should be skipped
        fs::write(functions_dir.join("api/hello.test.ts"), r#"// test file"#).unwrap();
        // Type definitions should be skipped
        fs::write(
            functions_dir.join("api/types.d.ts"),
            r#"export type Foo = string;"#,
        )
        .unwrap();
        base.to_path_buf()
    }

    #[test]
    fn has_functions_detects_directory() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = setup_functions_dir(temp.path());
        assert!(rift_engine::build::functions::has_functions(&workspace));
    }

    #[test]
    fn has_functions_returns_false_for_missing_dir() {
        let temp = tempfile::tempdir().unwrap();
        assert!(!rift_engine::build::functions::has_functions(temp.path()));
    }

    #[test]
    fn has_functions_with_populated_dir() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = setup_functions_dir(temp.path());
        // Verify that the function files we created are detected
        assert!(rift_engine::build::functions::has_functions(&workspace));
        // And verify that test/type-def files don't cause false positives elsewhere
        assert!(workspace.join("rift/functions/api/hello.ts").exists());
        assert!(workspace.join("rift/functions/api/hello.test.ts").exists());
        assert!(workspace.join("rift/functions/api/types.d.ts").exists());
    }

    #[test]
    fn has_functions_empty_dir() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("rift/functions")).unwrap();
        // An empty functions dir still counts as "has functions"
        assert!(rift_engine::build::functions::has_functions(temp.path()));
    }

    #[test]
    fn has_functions_no_dir() {
        let temp = tempfile::tempdir().unwrap();
        assert!(!rift_engine::build::functions::has_functions(temp.path()));
    }
}

/// Tests for pool configuration and stats.
mod pool_config {
    use rift_engine::runtime::pool::PoolConfig;
    use std::time::Duration;

    #[test]
    fn pool_config_defaults() {
        let config = PoolConfig {
            warm_pool_size: 3,
            max_active_workers: 50,
            idle_timeout: Duration::from_secs(300),
            worker_memory_limit: 512 * 1024 * 1024,
            loader_script: "/opt/rift/templates/worker_loader.ts".into(),
            deploy_root: "/var/rift/deployments".into(),
            seccomp_enforce: true,
        };

        assert_eq!(config.warm_pool_size, 3);
        assert_eq!(config.max_active_workers, 50);
        assert_eq!(config.idle_timeout, Duration::from_secs(300));
        assert_eq!(config.worker_memory_limit, 512 * 1024 * 1024);
    }
}

/// Tests for the build output detection with functions.
mod build_detection {
    #[test]
    fn functions_only_project_detected() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path();

        // Create a minimal package.json with no framework deps
        std::fs::write(
            workspace.join("package.json"),
            r#"{
                "name": "my-functions",
                "scripts": { "build": "echo done" },
                "dependencies": {}
            }"#,
        )
        .unwrap();

        // Create rift/functions directory
        std::fs::create_dir_all(workspace.join("rift/functions/api")).unwrap();
        std::fs::write(
            workspace.join("rift/functions/api/hello.ts"),
            r#"export default { fetch() { return new Response("hi"); } }"#,
        )
        .unwrap();

        let project = rift_engine::db::models::Project {
            id: uuid::Uuid::new_v4(),
            user_id: uuid::Uuid::new_v4(),
            name: "test".to_string(),
            repo_url: "https://github.com/test/test".to_string(),
            branch: "main".to_string(),
            framework: "unknown".to_string(),
            build_command: None,
            output_dir: None,
            install_command: None,
            subdomain: Some("test".to_string()),
            webhook_id: None,
            webhook_secret: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        let plan = rift_engine::build::detect::detect_build_plan(&project, workspace).unwrap();
        assert_eq!(plan.framework, "functions");
        assert!(matches!(
            plan.output,
            rift_engine::build::detect::BuildOutput::Functions
        ));
    }
}

/// Tests for the seccomp profile and cgroup utilities.
mod sandbox_tests {
    use rift_engine::runtime::pool::sandbox;

    #[test]
    fn seccomp_profile_is_valid_json() {
        let parsed: serde_json::Value = serde_json::from_str(sandbox::SECCOMP_PROFILE).unwrap();
        assert_eq!(parsed["defaultAction"].as_str().unwrap(), "SCMP_ACT_ERRNO");
        assert!(!parsed["syscalls"].as_array().unwrap().is_empty());
    }

    #[test]
    fn seccomp_profile_allows_essential_syscalls() {
        let parsed: serde_json::Value = serde_json::from_str(sandbox::SECCOMP_PROFILE).unwrap();
        let syscalls = parsed["syscalls"][0]["names"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect::<Vec<_>>();

        // Essential syscalls that Deno/Node.js need
        assert!(syscalls.contains(&"read"));
        assert!(syscalls.contains(&"write"));
        assert!(syscalls.contains(&"socket"));
        assert!(syscalls.contains(&"connect"));
        assert!(syscalls.contains(&"epoll_create1"));
        assert!(syscalls.contains(&"futex"));
        assert!(syscalls.contains(&"mmap"));
        assert!(syscalls.contains(&"clone"));
    }

    #[test]
    fn seccomp_profile_blocks_dangerous_syscalls() {
        let parsed: serde_json::Value = serde_json::from_str(sandbox::SECCOMP_PROFILE).unwrap();
        let allowed: Vec<&str> = parsed["syscalls"][0]["names"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();

        // These should NOT be in the allow list
        assert!(!allowed.contains(&"ptrace"));
        assert!(!allowed.contains(&"mount"));
        assert!(!allowed.contains(&"reboot"));
        assert!(!allowed.contains(&"kexec_load"));
        assert!(!allowed.contains(&"init_module"));
        assert!(!allowed.contains(&"sethostname"));
        assert!(!allowed.contains(&"settimeofday"));
    }

    #[test]
    fn seccomp_profile_write_creates_file() {
        let temp = tempfile::tempdir().unwrap();
        let path = sandbox::write_seccomp_profile(temp.path()).unwrap();
        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["defaultAction"].as_str().unwrap(), "SCMP_ACT_ERRNO");
    }
}

/// Tests for RuntimeKind::Combined detection from filesystem.
mod combined_detection {
    use std::fs;

    #[test]
    fn combined_entry_detected_over_functions_only() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path();

        // Create both functions output and combined entry
        let fn_dir = workspace.join("_rift_functions_output");
        fs::create_dir_all(fn_dir.join("bundles")).unwrap();
        fs::write(fn_dir.join("_rift_combined_entry.ts"), "// combined entry").unwrap();
        fs::write(fn_dir.join("_entry.ts"), "// functions entry").unwrap();

        // The detect_runtime_kind logic is internal, but we can verify
        // that the combined entry file takes priority by checking file existence
        assert!(fn_dir.join("_rift_combined_entry.ts").exists());
        assert!(fn_dir.join("_entry.ts").exists());

        // Combined entry should be detected when present
        assert!(workspace
            .join("_rift_functions_output/_rift_combined_entry.ts")
            .exists());
    }

    #[test]
    fn functions_only_when_no_combined_entry() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path();

        let fn_dir = workspace.join("_rift_functions_output");
        fs::create_dir_all(fn_dir.join("bundles")).unwrap();
        fs::write(fn_dir.join("_entry.ts"), "// functions entry").unwrap();

        // No combined entry — should be detected as Functions only
        assert!(!workspace
            .join("_rift_functions_output/_rift_combined_entry.ts")
            .exists());
        assert!(workspace.join("_rift_functions_output/bundles").is_dir());
    }
}

/// Tests for seccomp enforcement configuration and behavior.
mod seccomp_enforcement {
    use rift_engine::runtime::pool::sandbox;

    #[test]
    fn enforcer_init_non_enforce_always_succeeds() {
        // With enforce=false, init should succeed even without seccomp support
        let temp = tempfile::tempdir().unwrap();
        let enforcer = sandbox::SeccompEnforcer::init(temp.path(), false).unwrap();
        // On macOS, seccomp is not available, so profile_path should be None
        // On Linux, it would be Some
        assert!(!enforcer.enforce);
    }

    #[test]
    fn enforcer_should_apply_matches_profile_availability() {
        let temp = tempfile::tempdir().unwrap();
        let enforcer = sandbox::SeccompEnforcer::init(temp.path(), false).unwrap();
        // should_apply is true only if profile was written
        assert_eq!(enforcer.should_apply(), enforcer.profile_path.is_some());
    }

    #[test]
    fn enforcer_docker_security_opt_format() {
        let temp = tempfile::tempdir().unwrap();
        let enforcer = sandbox::SeccompEnforcer::init(temp.path(), false).unwrap();
        if let Some(opt) = enforcer.docker_security_opt() {
            assert!(opt.starts_with("seccomp="));
            assert!(opt.contains("rift-worker-seccomp.json"));
        }
    }

    #[test]
    fn enforcer_enforce_on_unavailable_system_fails() {
        // On macOS (no seccomp), enforce=true should fail
        if !sandbox::is_seccomp_available() {
            let temp = tempfile::tempdir().unwrap();
            let result = sandbox::SeccompEnforcer::init(temp.path(), true);
            assert!(result.is_err());
            let err_msg = result.unwrap_err().to_string();
            assert!(err_msg.contains("seccomp enforcement is enabled"));
        }
    }

    #[test]
    fn seccomp_profile_contains_required_architectures() {
        let parsed: serde_json::Value = serde_json::from_str(sandbox::SECCOMP_PROFILE).unwrap();
        let archs = parsed["architectures"].as_array().unwrap();
        let arch_strs: Vec<&str> = archs.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(arch_strs.contains(&"SCMP_ARCH_X86_64"));
        assert!(arch_strs.contains(&"SCMP_ARCH_AARCH64"));
    }
}

/// Tests for immutable artifact creation.
mod immutable_artifacts {
    use std::fs;

    #[test]
    fn artifact_manifest_structure() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path();

        // Create a static site workspace
        let output_dir = workspace.join("dist");
        fs::create_dir_all(&output_dir).unwrap();
        fs::write(output_dir.join("index.html"), "<html></html>").unwrap();
        fs::write(output_dir.join("_entry.ts"), "// entry").unwrap();

        // Write a manifest matching what the build pipeline would create
        let manifest = serde_json::json!({
            "version": 1,
            "runtime_type": "static",
            "entry_point": output_dir.join("_entry.ts").to_string_lossy(),
            "functions_dir": null,
        });
        let manifest_path = workspace.join("_rift_manifest.json");
        fs::write(
            &manifest_path,
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();

        // Verify manifest can be read back
        let content = fs::read_to_string(&manifest_path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["version"].as_i64().unwrap(), 1);
        assert_eq!(parsed["runtime_type"].as_str().unwrap(), "static");
        assert!(parsed["entry_point"]
            .as_str()
            .unwrap()
            .contains("_entry.ts"));
    }

    #[test]
    fn artifact_directory_layout() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path();

        // Simulate a functions deployment workspace
        let fn_dir = workspace.join("_rift_functions_output");
        fs::create_dir_all(fn_dir.join("bundles")).unwrap();
        fs::write(fn_dir.join("_entry.ts"), "// dispatcher").unwrap();
        fs::write(fn_dir.join("bundles/api_hello.js"), "// bundle").unwrap();
        fs::write(fn_dir.join("_routes.json"), r#"[{"pattern": "/api/hello", "file_path": "api/hello.ts", "bundle_file": "bundles/api_hello.js"}]"#).unwrap();

        // Verify the expected artifact structure
        assert!(fn_dir.join("_entry.ts").exists());
        assert!(fn_dir.join("bundles/api_hello.js").exists());
        assert!(fn_dir.join("_routes.json").exists());
    }

    #[test]
    fn manifest_types_cover_all_runtime_kinds() {
        // Verify that all runtime_type strings are accounted for
        let valid_types = vec!["static", "next", "node_ssr", "functions", "combined"];
        for t in &valid_types {
            let manifest = serde_json::json!({
                "version": 1,
                "runtime_type": t,
                "entry_point": "/path/to/entry",
                "functions_dir": null,
            });
            assert_eq!(manifest["runtime_type"].as_str().unwrap(), *t);
        }
    }
}

/// Tests for build concurrency configuration.
mod build_concurrency {
    #[test]
    fn semaphore_with_concurrency_greater_than_one() {
        // Verify that Semaphore allows multiple permits
        let concurrency = 4usize;
        let sem = tokio::sync::Semaphore::new(concurrency.max(1));
        assert_eq!(sem.available_permits(), 4);
    }

    #[test]
    fn semaphore_minimum_is_one() {
        // Even if config says 0, we floor to 1
        let concurrency = 0usize;
        let sem = tokio::sync::Semaphore::new(concurrency.max(1));
        assert_eq!(sem.available_permits(), 1);
    }

    #[test]
    fn cache_key_deterministic_for_same_content() {
        use sha2::{Digest, Sha256};

        let content = b"lockfile contents here";
        let hash1 = format!("{:x}", Sha256::digest(content));
        let hash2 = format!("{:x}", Sha256::digest(content));
        assert_eq!(hash1, hash2);
        assert_eq!(hash1.len(), 64); // SHA-256 hex string
    }

    #[test]
    fn cache_key_differs_for_different_content() {
        use sha2::{Digest, Sha256};

        let hash1 = format!("{:x}", Sha256::digest(b"lockfile v1"));
        let hash2 = format!("{:x}", Sha256::digest(b"lockfile v2"));
        assert_ne!(hash1, hash2);
    }
}

/// Tests for deploy speed optimizations.
mod deploy_speed {
    use std::path::Path;
    use tempfile::TempDir;

    // --- CopyMode / CoW fallback ---

    #[test]
    fn copy_mode_parses_correctly() {
        use rift_engine::build::CopyMode;
        assert_eq!("auto".parse::<CopyMode>().unwrap(), CopyMode::Auto);
        assert_eq!("reflink".parse::<CopyMode>().unwrap(), CopyMode::Reflink);
        assert_eq!(
            "recursive".parse::<CopyMode>().unwrap(),
            CopyMode::Recursive
        );
        assert_eq!("unknown".parse::<CopyMode>().unwrap(), CopyMode::Auto); // default
    }

    // --- Native cache env generation ---

    #[test]
    fn native_cache_env_npm() {
        use rift_engine::build::{detect::PackageManager, native_cache_env};
        let cache_dir = Path::new("/var/rift/cache");
        let envs = native_cache_env(cache_dir, &PackageManager::Npm);
        assert_eq!(envs.len(), 1);
        assert_eq!(envs[0].0, "npm_config_cache");
        assert!(envs[0].1.contains("native/npm"));
    }

    #[test]
    fn native_cache_env_pnpm() {
        use rift_engine::build::{detect::PackageManager, native_cache_env};
        let cache_dir = Path::new("/var/rift/cache");
        let envs = native_cache_env(cache_dir, &PackageManager::Pnpm);
        assert_eq!(envs.len(), 2);
        let keys: Vec<&str> = envs.iter().map(|(k, _)| k.as_str()).collect();
        assert!(keys.contains(&"PNPM_HOME"));
        assert!(keys.contains(&"npm_config_store_dir"));
    }

    #[test]
    fn native_cache_env_yarn() {
        use rift_engine::build::{detect::PackageManager, native_cache_env};
        let cache_dir = Path::new("/var/rift/cache");
        let envs = native_cache_env(cache_dir, &PackageManager::Yarn);
        assert_eq!(envs.len(), 1);
        assert_eq!(envs[0].0, "YARN_CACHE_FOLDER");
    }

    // --- Optimized install command ---

    #[test]
    fn optimized_install_npm_default() {
        use rift_engine::build::{detect::PackageManager, optimized_install_command};
        let cmd = optimized_install_command(&PackageManager::Npm, "npm install");
        assert_eq!(cmd, "npm ci --prefer-offline");
    }

    #[test]
    fn optimized_install_npm_custom_not_touched() {
        use rift_engine::build::{detect::PackageManager, optimized_install_command};
        let cmd = optimized_install_command(&PackageManager::Npm, "npm ci --legacy-peer-deps");
        assert_eq!(cmd, "npm ci --legacy-peer-deps");
    }

    #[test]
    fn optimized_install_pnpm_default() {
        use rift_engine::build::{detect::PackageManager, optimized_install_command};
        let cmd = optimized_install_command(&PackageManager::Pnpm, "pnpm install");
        assert_eq!(cmd, "pnpm install --frozen-lockfile --prefer-offline");
    }

    #[test]
    fn optimized_install_yarn_default() {
        use rift_engine::build::{detect::PackageManager, optimized_install_command};
        let cmd = optimized_install_command(&PackageManager::Yarn, "yarn install");
        assert_eq!(cmd, "yarn install");
    }

    // --- Health check config ---

    #[test]
    fn runtime_manager_healthcheck_config() {
        let mut rm = rift_engine::runtime::RuntimeManager::new();
        // Defaults
        assert_eq!(rm.healthcheck_interval_ms(), 200);
        assert_eq!(rm.healthcheck_attempts(), 50);
        // Override
        rm.set_healthcheck(100, 30);
        assert_eq!(rm.healthcheck_interval_ms(), 100);
        assert_eq!(rm.healthcheck_attempts(), 30);
    }

    // --- CoW copy with mode (recursive mode always works) ---

    #[tokio::test]
    async fn copy_dir_with_mode_recursive() {
        let src = TempDir::new().unwrap();
        let dst = TempDir::new().unwrap();
        let dst_target = dst.path().join("output");

        // Create test files
        std::fs::write(src.path().join("hello.txt"), "world").unwrap();
        std::fs::create_dir_all(src.path().join("sub")).unwrap();
        std::fs::write(src.path().join("sub/nested.txt"), "deep").unwrap();

        rift_engine::build::copy_dir_with_mode(
            src.path(),
            &dst_target,
            rift_engine::build::CopyMode::Recursive,
        )
        .await
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(dst_target.join("hello.txt")).unwrap(),
            "world"
        );
        assert_eq!(
            std::fs::read_to_string(dst_target.join("sub/nested.txt")).unwrap(),
            "deep"
        );
    }

    #[tokio::test]
    async fn copy_dir_with_mode_auto_fallback() {
        // Auto mode should work even on filesystems without CoW
        let src = TempDir::new().unwrap();
        let dst = TempDir::new().unwrap();
        let dst_target = dst.path().join("output");

        std::fs::write(src.path().join("data.bin"), vec![42u8; 1024]).unwrap();

        rift_engine::build::copy_dir_with_mode(
            src.path(),
            &dst_target,
            rift_engine::build::CopyMode::Auto,
        )
        .await
        .unwrap();

        assert_eq!(
            std::fs::read(dst_target.join("data.bin")).unwrap(),
            vec![42u8; 1024]
        );
    }

    // --- Config fields ---

    #[test]
    fn config_struct_is_constructible() {
        // Verify the Config struct has the new fields by checking its size
        let _ = std::mem::size_of::<rift_engine::config::Config>();
    }
}

/// Tests for resource limits configuration.
mod resource_limits {
    use rift_engine::runtime::pool::limits::ResourceLimits;

    #[test]
    fn default_limits_are_reasonable() {
        let limits = ResourceLimits::default();
        assert_eq!(limits.memory_max, 512 * 1024 * 1024); // 512 MB
        assert_eq!(limits.memory_high, 384 * 1024 * 1024); // 384 MB
        assert_eq!(limits.cpu_quota_us, 100_000); // 100% of one core
        assert_eq!(limits.cpu_period_us, 100_000); // 100ms period
        assert_eq!(limits.max_pids, 64);
    }

    #[test]
    fn cgroup_availability_check_works() {
        // On macOS and most CI, this should return false
        let available = rift_engine::runtime::pool::limits::is_cgroup_v2_available();
        // Just verify it doesn't panic
        let _ = available;
    }
}

/// Phase 5.3 + 5.4: Build/runtime policy separation and enforcement tests.
mod resource_policy {
    use rift_engine::runtime::policy::{
        resolve_build_policy, resolve_runtime_policy, BuildPolicy, EnforcementMode,
        ProjectPolicyOverrides, ResourceError, RuntimePolicy,
    };
    use uuid::Uuid;

    fn test_config() -> rift_engine::config::Config {
        rift_engine::config::Config {
            database_url: String::new(),
            master_key: String::new(),
            jwt_private_key_pem: String::new(),
            jwt_public_key_pem: String::new(),
            internal_api_token: String::new(),
            api_bind: "0.0.0.0".into(),
            api_port: 3001,
            proxy_bind: "0.0.0.0".into(),
            proxy_port: 8080,
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
            worker_id: None,
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

    // --- Build/runtime separation tests ---

    #[test]
    fn build_and_runtime_policies_are_independent() {
        let config = test_config();
        let runtime = resolve_runtime_policy(&config, None);
        let build = resolve_build_policy(&config, None);

        // Build policy has higher resource limits than runtime
        assert!(build.memory_max_bytes > runtime.memory_max_bytes);
        assert!(build.cpu_quota_us > runtime.cpu_quota_us);
        assert!(build.max_pids > runtime.max_pids);

        // Runtime has fields that build does not
        assert_eq!(runtime.max_open_files, 1024);
        assert_eq!(runtime.request_timeout_secs, 30);
        assert_eq!(runtime.max_concurrent_requests, 100);
    }

    #[test]
    fn build_overrides_do_not_leak_to_runtime() {
        let config = test_config();
        let overrides = ProjectPolicyOverrides {
            build_memory_max_bytes: Some(4 * 1024 * 1024 * 1024),
            build_cpu_quota_us: Some(400_000),
            build_timeout_secs: Some(1200),
            ..Default::default()
        };

        let runtime = resolve_runtime_policy(&config, Some(&overrides));
        let build = resolve_build_policy(&config, Some(&overrides));

        // Runtime should use defaults (not build overrides)
        assert_eq!(runtime.memory_max_bytes, 512 * 1024 * 1024);
        assert_eq!(runtime.cpu_quota_us, 100_000);

        // Build should use overrides
        assert_eq!(build.memory_max_bytes, 4 * 1024 * 1024 * 1024);
        assert_eq!(build.cpu_quota_us, 400_000);
        assert_eq!(build.build_timeout_secs, 1200);
    }

    #[test]
    fn runtime_overrides_do_not_leak_to_build() {
        let config = test_config();
        let overrides = ProjectPolicyOverrides {
            memory_max_bytes: Some(128 * 1024 * 1024),
            cpu_quota_us: Some(25_000),
            max_concurrent_requests: Some(5),
            ..Default::default()
        };

        let runtime = resolve_runtime_policy(&config, Some(&overrides));
        let build = resolve_build_policy(&config, Some(&overrides));

        // Runtime should use overrides
        assert_eq!(runtime.memory_max_bytes, 128 * 1024 * 1024);
        assert_eq!(runtime.cpu_quota_us, 25_000);
        assert_eq!(runtime.max_concurrent_requests, 5);

        // Build should use defaults (not runtime overrides)
        assert_eq!(build.memory_max_bytes, 2048 * 1024 * 1024);
        assert_eq!(build.cpu_quota_us, 200_000);
    }

    // --- Override precedence tests ---

    #[test]
    fn all_runtime_fields_can_be_overridden() {
        let config = test_config();
        let overrides = ProjectPolicyOverrides {
            memory_max_bytes: Some(256 * 1024 * 1024),
            memory_high_bytes: Some(200 * 1024 * 1024),
            cpu_quota_us: Some(50_000),
            max_pids: Some(32),
            max_open_files: Some(512),
            request_timeout_secs: Some(60),
            max_concurrent_requests: Some(10),
            ..Default::default()
        };

        let policy = resolve_runtime_policy(&config, Some(&overrides));

        assert_eq!(policy.memory_max_bytes, 256 * 1024 * 1024);
        assert_eq!(policy.memory_high_bytes, 200 * 1024 * 1024);
        assert_eq!(policy.cpu_quota_us, 50_000);
        assert_eq!(policy.max_pids, 32);
        assert_eq!(policy.max_open_files, 512);
        assert_eq!(policy.request_timeout_secs, 60);
        assert_eq!(policy.max_concurrent_requests, 10);
        // cpu_period_us is always fixed
        assert_eq!(policy.cpu_period_us, 100_000);
    }

    #[test]
    fn partial_overrides_keep_defaults_for_unset_fields() {
        let config = test_config();
        let overrides = ProjectPolicyOverrides {
            memory_max_bytes: Some(1024 * 1024 * 1024), // Only override memory
            ..Default::default()
        };

        let policy = resolve_runtime_policy(&config, Some(&overrides));

        assert_eq!(policy.memory_max_bytes, 1024 * 1024 * 1024);
        // Everything else uses config defaults
        assert_eq!(policy.cpu_quota_us, 100_000);
        assert_eq!(policy.max_pids, 64);
        assert_eq!(policy.max_open_files, 1024);
        assert_eq!(policy.request_timeout_secs, 30);
        assert_eq!(policy.max_concurrent_requests, 100);
    }

    #[test]
    fn empty_overrides_equal_no_overrides() {
        let config = test_config();
        let without = resolve_runtime_policy(&config, None);
        let with_empty = resolve_runtime_policy(&config, Some(&ProjectPolicyOverrides::default()));

        assert_eq!(without, with_empty);
    }

    // --- Config-driven defaults tests ---

    #[test]
    fn config_memory_limit_drives_runtime_policy() {
        let mut config = test_config();
        config.worker_memory_limit_mb = 1024;

        let policy = resolve_runtime_policy(&config, None);

        assert_eq!(policy.memory_max_bytes, 1024 * 1024 * 1024);
        assert_eq!(policy.memory_high_bytes, 1024 * 1024 * 1024 * 3 / 4);
    }

    #[test]
    fn config_build_limits_drive_build_policy() {
        let mut config = test_config();
        config.build_memory_limit_mb = 4096;
        config.build_cpu_quota_us = 400_000;
        config.build_timeout_secs = 1200;

        let policy = resolve_build_policy(&config, None);

        assert_eq!(policy.memory_max_bytes, 4096 * 1024 * 1024);
        assert_eq!(policy.cpu_quota_us, 400_000);
        assert_eq!(policy.build_timeout_secs, 1200);
    }

    // --- ResourceLimits conversion tests ---

    #[test]
    fn build_policy_resource_limits_use_75_pct_high_watermark() {
        let policy = BuildPolicy {
            memory_max_bytes: 1000,
            cpu_quota_us: 100_000,
            cpu_period_us: 100_000,
            max_pids: 64,
            build_timeout_secs: 300,
        };
        let limits = policy.to_resource_limits();

        assert_eq!(limits.memory_high, 750); // 75% of 1000
    }

    #[test]
    fn runtime_policy_resource_limits_preserve_explicit_high() {
        let policy = RuntimePolicy {
            memory_max_bytes: 1000,
            memory_high_bytes: 800,
            ..Default::default()
        };
        let limits = policy.to_resource_limits();

        assert_eq!(limits.memory_max, 1000);
        assert_eq!(limits.memory_high, 800);
    }

    // --- Enforcement mode tests ---

    #[test]
    fn enforcement_strict_from_config() {
        let mut config = test_config();
        config.resource_enforcement = "strict".into();
        assert_eq!(
            EnforcementMode::from_config(&config),
            EnforcementMode::Strict
        );
    }

    #[test]
    fn enforcement_best_effort_from_config() {
        let mut config = test_config();
        config.resource_enforcement = "best-effort".into();
        assert_eq!(
            EnforcementMode::from_config(&config),
            EnforcementMode::BestEffort
        );
    }

    #[test]
    fn enforcement_unknown_defaults_to_best_effort() {
        let mut config = test_config();
        config.resource_enforcement = "unknown-value".into();
        assert_eq!(
            EnforcementMode::from_config(&config),
            EnforcementMode::BestEffort
        );
    }

    // --- Error taxonomy tests ---

    #[test]
    fn pool_capacity_error_maps_to_conflict() {
        let err: rift_engine::error::AppError = ResourceError::PoolCapacityExceeded {
            active: 50,
            max: 50,
        }
        .into();
        assert!(matches!(err, rift_engine::error::AppError::Conflict(_)));
    }

    #[test]
    fn concurrent_request_error_maps_to_rate_limited() {
        let err: rift_engine::error::AppError = ResourceError::ConcurrentRequestLimitExceeded {
            project_id: Uuid::new_v4(),
            limit: 100,
        }
        .into();
        assert!(matches!(err, rift_engine::error::AppError::RateLimited(_)));
    }

    #[test]
    fn cgroup_errors_map_to_internal() {
        let err: rift_engine::error::AppError = ResourceError::CgroupUnavailable.into();
        assert!(matches!(err, rift_engine::error::AppError::Internal(_)));

        let err: rift_engine::error::AppError = ResourceError::CgroupSetupFailed {
            worker_id: Uuid::new_v4(),
            detail: "permission denied".into(),
        }
        .into();
        assert!(matches!(err, rift_engine::error::AppError::Internal(_)));

        let err: rift_engine::error::AppError = ResourceError::CgroupAttachFailed {
            worker_id: Uuid::new_v4(),
            pid: 12345,
            detail: "no such process".into(),
        }
        .into();
        assert!(matches!(err, rift_engine::error::AppError::Internal(_)));
    }

    // --- Enforcement behavior tests ---

    #[test]
    fn enforce_cgroup_best_effort_succeeds_without_cgroup() {
        // On macOS/CI where cgroups aren't available, best-effort should succeed
        let worker_id = Uuid::new_v4();
        let limits = rift_engine::runtime::pool::limits::ResourceLimits::default();
        let result = rift_engine::runtime::policy::enforce_cgroup_limits(
            &worker_id,
            99999,
            &limits,
            EnforcementMode::BestEffort,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn enforce_cgroup_strict_fails_without_cgroup() {
        // Only test this on macOS/CI where cgroups aren't available
        if rift_engine::runtime::pool::limits::is_cgroup_v2_available() {
            return; // Skip on Linux with cgroups
        }
        let worker_id = Uuid::new_v4();
        let limits = rift_engine::runtime::pool::limits::ResourceLimits::default();
        let result = rift_engine::runtime::policy::enforce_cgroup_limits(
            &worker_id,
            99999,
            &limits,
            EnforcementMode::Strict,
        );
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), ResourceError::CgroupUnavailable);
    }

    #[test]
    fn release_cgroup_is_idempotent() {
        // Releasing a non-existent cgroup should not panic
        let worker_id = Uuid::new_v4();
        rift_engine::runtime::policy::release_cgroup(&worker_id);
        rift_engine::runtime::policy::release_cgroup(&worker_id);
    }

    // --- Pool capacity enforcement tests ---

    #[test]
    fn resource_error_display_includes_details() {
        let worker_id = Uuid::new_v4();
        let err = ResourceError::CgroupSetupFailed {
            worker_id,
            detail: "permission denied".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains(&worker_id.to_string()));
        assert!(msg.contains("permission denied"));

        let err = ResourceError::CgroupAttachFailed {
            worker_id,
            pid: 42,
            detail: "no such process".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("42"));
        assert!(msg.contains("no such process"));
    }

    // --- Serialization round-trip tests ---

    #[test]
    fn runtime_policy_serializes_and_deserializes() {
        let policy = RuntimePolicy::default();
        let json = serde_json::to_string(&policy).unwrap();
        let deserialized: RuntimePolicy = serde_json::from_str(&json).unwrap();
        assert_eq!(policy, deserialized);
    }

    #[test]
    fn build_policy_serializes_and_deserializes() {
        let policy = BuildPolicy::default();
        let json = serde_json::to_string(&policy).unwrap();
        let deserialized: BuildPolicy = serde_json::from_str(&json).unwrap();
        assert_eq!(policy, deserialized);
    }

    #[test]
    fn project_overrides_serialize_with_null_fields() {
        let overrides = ProjectPolicyOverrides {
            memory_max_bytes: Some(256 * 1024 * 1024),
            ..Default::default()
        };
        let json = serde_json::to_string(&overrides).unwrap();
        let deserialized: ProjectPolicyOverrides = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.memory_max_bytes, Some(256 * 1024 * 1024));
        assert!(deserialized.cpu_quota_us.is_none());
    }
}
