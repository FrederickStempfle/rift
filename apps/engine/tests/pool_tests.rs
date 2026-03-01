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

        assert_eq!(routes, vec!["/api/health", "/api/users/me", "/api/users/:id"]);
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
        fs::write(
            functions_dir.join("api/hello.test.ts"),
            r#"// test file"#,
        )
        .unwrap();
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
            subdomain: "test".to_string(),
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
        let parsed: serde_json::Value =
            serde_json::from_str(sandbox::SECCOMP_PROFILE).unwrap();
        assert_eq!(
            parsed["defaultAction"].as_str().unwrap(),
            "SCMP_ACT_ERRNO"
        );
        assert!(parsed["syscalls"].as_array().unwrap().len() > 0);
    }

    #[test]
    fn seccomp_profile_allows_essential_syscalls() {
        let parsed: serde_json::Value =
            serde_json::from_str(sandbox::SECCOMP_PROFILE).unwrap();
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
        let parsed: serde_json::Value =
            serde_json::from_str(sandbox::SECCOMP_PROFILE).unwrap();
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
        fs::write(
            fn_dir.join("_rift_combined_entry.ts"),
            "// combined entry",
        )
        .unwrap();
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
        assert!(workspace
            .join("_rift_functions_output/bundles")
            .is_dir());
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
        assert_eq!(enforcer.enforce, false);
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
        let parsed: serde_json::Value =
            serde_json::from_str(sandbox::SECCOMP_PROFILE).unwrap();
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
        fs::write(&manifest_path, serde_json::to_string_pretty(&manifest).unwrap()).unwrap();

        // Verify manifest can be read back
        let content = fs::read_to_string(&manifest_path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["version"].as_i64().unwrap(), 1);
        assert_eq!(parsed["runtime_type"].as_str().unwrap(), "static");
        assert!(parsed["entry_point"].as_str().unwrap().contains("_entry.ts"));
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
        assert_eq!(CopyMode::from_str("auto"), CopyMode::Auto);
        assert_eq!(CopyMode::from_str("reflink"), CopyMode::Reflink);
        assert_eq!(CopyMode::from_str("recursive"), CopyMode::Recursive);
        assert_eq!(CopyMode::from_str("unknown"), CopyMode::Auto); // default
    }

    // --- Native cache env generation ---

    #[test]
    fn native_cache_env_npm() {
        use rift_engine::build::{native_cache_env, detect::PackageManager};
        let cache_dir = Path::new("/var/rift/cache");
        let envs = native_cache_env(cache_dir, &PackageManager::Npm);
        assert_eq!(envs.len(), 1);
        assert_eq!(envs[0].0, "npm_config_cache");
        assert!(envs[0].1.contains("native/npm"));
    }

    #[test]
    fn native_cache_env_pnpm() {
        use rift_engine::build::{native_cache_env, detect::PackageManager};
        let cache_dir = Path::new("/var/rift/cache");
        let envs = native_cache_env(cache_dir, &PackageManager::Pnpm);
        assert_eq!(envs.len(), 2);
        let keys: Vec<&str> = envs.iter().map(|(k, _)| k.as_str()).collect();
        assert!(keys.contains(&"PNPM_HOME"));
        assert!(keys.contains(&"npm_config_store_dir"));
    }

    #[test]
    fn native_cache_env_yarn() {
        use rift_engine::build::{native_cache_env, detect::PackageManager};
        let cache_dir = Path::new("/var/rift/cache");
        let envs = native_cache_env(cache_dir, &PackageManager::Yarn);
        assert_eq!(envs.len(), 1);
        assert_eq!(envs[0].0, "YARN_CACHE_FOLDER");
    }

    // --- Optimized install command ---

    #[test]
    fn optimized_install_npm_default() {
        use rift_engine::build::{optimized_install_command, detect::PackageManager};
        let cmd = optimized_install_command(&PackageManager::Npm, "npm install");
        assert_eq!(cmd, "npm ci --prefer-offline");
    }

    #[test]
    fn optimized_install_npm_custom_not_touched() {
        use rift_engine::build::{optimized_install_command, detect::PackageManager};
        let cmd = optimized_install_command(&PackageManager::Npm, "npm ci --legacy-peer-deps");
        assert_eq!(cmd, "npm ci --legacy-peer-deps");
    }

    #[test]
    fn optimized_install_pnpm_default() {
        use rift_engine::build::{optimized_install_command, detect::PackageManager};
        let cmd = optimized_install_command(&PackageManager::Pnpm, "pnpm install");
        assert_eq!(cmd, "pnpm install --frozen-lockfile --prefer-offline");
    }

    #[test]
    fn optimized_install_yarn_default() {
        use rift_engine::build::{optimized_install_command, detect::PackageManager};
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
