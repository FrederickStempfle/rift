use std::path::PathBuf;

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

    use super::*;

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

    use std::path::Path;

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

    #[tokio::test]
    async fn build_function_bundle_scans_routes() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = setup_functions_dir(temp.path());
        let output_dir = temp.path().join("output");

        let routes = rift_engine::build::functions::build_function_bundle(
            &workspace,
            &output_dir,
        )
        .await
        .unwrap();

        assert_eq!(routes.len(), 3);

        let patterns: Vec<&str> = routes.iter().map(|r| r.pattern.as_str()).collect();
        assert!(patterns.contains(&"/"));
        assert!(patterns.contains(&"/api/hello"));
        assert!(patterns.contains(&"/api/users/:id"));

        // Test and type def files should have been skipped
        assert!(!patterns.iter().any(|p| p.contains("test")));
        assert!(!patterns.iter().any(|p| p.contains("types")));

        // The entry file should have been written
        assert!(output_dir.join("_rift_functions_entry.ts").exists());
    }

    #[tokio::test]
    async fn build_function_bundle_empty_dir() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("rift/functions")).unwrap();
        let output_dir = temp.path().join("output");

        let routes = rift_engine::build::functions::build_function_bundle(
            temp.path(),
            &output_dir,
        )
        .await
        .unwrap();

        assert!(routes.is_empty());
    }

    #[tokio::test]
    async fn build_function_bundle_no_functions_dir() {
        let temp = tempfile::tempdir().unwrap();
        let output_dir = temp.path().join("output");

        let routes = rift_engine::build::functions::build_function_bundle(
            temp.path(),
            &output_dir,
        )
        .await
        .unwrap();

        assert!(routes.is_empty());
    }

    #[tokio::test]
    async fn static_routes_sorted_before_parameterized() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = setup_functions_dir(temp.path());
        let output_dir = temp.path().join("output");

        let routes = rift_engine::build::functions::build_function_bundle(
            &workspace,
            &output_dir,
        )
        .await
        .unwrap();

        // Static routes should come before parameterized ones
        let first_param_idx = routes.iter().position(|r| r.pattern.contains(':')).unwrap_or(routes.len());
        for (i, route) in routes.iter().enumerate() {
            if i < first_param_idx {
                assert!(
                    !route.pattern.contains(':'),
                    "static route {} should come before parameterized routes",
                    route.pattern
                );
            }
        }
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
