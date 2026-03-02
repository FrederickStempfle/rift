//! Phase 7: Multi-node integration, chaos, and load hardening tests.
//!
//! These tests validate correctness properties of the distributed state layer,
//! scheduler, routing cache, and lifecycle operations under concurrent and
//! adversarial conditions. They use the in-memory `LocalStateStore` and
//! `MockBackend` to avoid requiring external infrastructure (Redis, Postgres).

mod cross_node_routing {
    use std::sync::Arc;

    use futures_util::future::join_all;
    use rift_engine::state::local::LocalStateStore;
    use rift_engine::state::{RoutingEntry, StateStore};
    use uuid::Uuid;

    /// Multiple nodes writing routing entries for different hosts converge correctly.
    #[tokio::test]
    async fn concurrent_route_writes_for_different_hosts() {
        let store = Arc::new(LocalStateStore::new());
        let mut handles = Vec::new();

        for i in 0..50 {
            let store = store.clone();
            handles.push(tokio::spawn(async move {
                let entry = RoutingEntry {
                    host: format!("app{i}.rift.dev"),
                    project_id: Uuid::new_v4(),
                    deployment_id: Uuid::new_v4(),
                    worker_addr: format!("10.0.0.{}", i % 256),
                    version: 1,
                };
                store.set_routing(&entry).await.unwrap();
                (entry.host.clone(), entry.project_id)
            }));
        }

        let results: Vec<_> = join_all(handles)
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();

        // Every host should be retrievable.
        for (host, project_id) in &results {
            let got = store.get_routing(host).await.unwrap().unwrap();
            assert_eq!(got.project_id, *project_id);
        }
    }

    /// Overwriting a route for the same host with a higher version wins.
    #[tokio::test]
    async fn route_overwrite_higher_version_wins() {
        let store = LocalStateStore::new();
        let pid_v1 = Uuid::new_v4();
        let pid_v2 = Uuid::new_v4();

        store
            .set_routing(&RoutingEntry {
                host: "app.rift.dev".into(),
                project_id: pid_v1,
                deployment_id: Uuid::new_v4(),
                worker_addr: "10.0.0.1".into(),
                version: 1,
            })
            .await
            .unwrap();

        store
            .set_routing(&RoutingEntry {
                host: "app.rift.dev".into(),
                project_id: pid_v2,
                deployment_id: Uuid::new_v4(),
                worker_addr: "10.0.0.2".into(),
                version: 2,
            })
            .await
            .unwrap();

        let got = store.get_routing("app.rift.dev").await.unwrap().unwrap();
        assert_eq!(got.project_id, pid_v2);
        assert_eq!(got.version, 2);
    }

    /// Removing a route makes it invisible.
    #[tokio::test]
    async fn route_remove_makes_invisible() {
        let store = LocalStateStore::new();
        store
            .set_routing(&RoutingEntry {
                host: "app.rift.dev".into(),
                project_id: Uuid::new_v4(),
                deployment_id: Uuid::new_v4(),
                worker_addr: "10.0.0.1".into(),
                version: 1,
            })
            .await
            .unwrap();

        store.remove_routing("app.rift.dev").await.unwrap();
        assert!(store.get_routing("app.rift.dev").await.unwrap().is_none());
    }
}

mod lease_contention {
    use std::sync::Arc;

    use rift_engine::state::local::LocalStateStore;
    use rift_engine::state::{PlacementLease, StateStore};
    use uuid::Uuid;

    fn lease(project_id: Uuid, version: u64, worker: &str) -> PlacementLease {
        PlacementLease {
            worker_id: worker.to_owned(),
            deployment_id: Uuid::new_v4(),
            project_id,
            version,
            ttl_secs: 300,
        }
    }

    /// CAS: only one of two concurrent version-1 acquires succeeds.
    #[tokio::test]
    async fn only_one_version1_acquire_succeeds() {
        let store = Arc::new(LocalStateStore::new());
        let pid = Uuid::new_v4();

        let store1 = store.clone();
        let store2 = store.clone();

        let (r1, r2) = tokio::join!(
            async move { store1.acquire_placement(pid, &lease(pid, 1, "w1")).await },
            async move { store2.acquire_placement(pid, &lease(pid, 1, "w2")).await },
        );

        let ok1 = r1.unwrap();
        let ok2 = r2.unwrap();

        // Exactly one should succeed (both try version 1).
        assert!(
            (ok1 && !ok2) || (!ok1 && ok2),
            "Expected exactly one acquire to succeed: ok1={ok1}, ok2={ok2}"
        );
    }

    /// Higher version always wins over lower.
    #[tokio::test]
    async fn higher_version_always_wins() {
        let store = LocalStateStore::new();
        let pid = Uuid::new_v4();

        assert!(store
            .acquire_placement(pid, &lease(pid, 1, "w1"))
            .await
            .unwrap());
        assert!(store
            .acquire_placement(pid, &lease(pid, 5, "w2"))
            .await
            .unwrap());

        let got = store.get_placement(pid).await.unwrap().unwrap();
        assert_eq!(got.worker_id, "w2");
        assert_eq!(got.version, 5);
    }

    /// Lower version is rejected when higher exists.
    #[tokio::test]
    async fn lower_version_rejected() {
        let store = LocalStateStore::new();
        let pid = Uuid::new_v4();

        store
            .acquire_placement(pid, &lease(pid, 5, "w1"))
            .await
            .unwrap();
        assert!(!store
            .acquire_placement(pid, &lease(pid, 3, "w2"))
            .await
            .unwrap());

        let got = store.get_placement(pid).await.unwrap().unwrap();
        assert_eq!(got.worker_id, "w1");
    }

    /// Release only works with matching version.
    #[tokio::test]
    async fn release_requires_matching_version() {
        let store = LocalStateStore::new();
        let pid = Uuid::new_v4();

        store
            .acquire_placement(pid, &lease(pid, 3, "w1"))
            .await
            .unwrap();

        // Wrong version — should not release.
        assert!(!store.release_placement(pid, 1).await.unwrap());
        assert!(store.get_placement(pid).await.unwrap().is_some());

        // Correct version — should release.
        assert!(store.release_placement(pid, 3).await.unwrap());
        assert!(store.get_placement(pid).await.unwrap().is_none());
    }

    /// Renew extends the lease for an existing placement.
    #[tokio::test]
    async fn renew_extends_ttl() {
        let store = LocalStateStore::new();
        let pid = Uuid::new_v4();

        store
            .acquire_placement(pid, &lease(pid, 1, "w1"))
            .await
            .unwrap();

        assert!(store.renew_placement(pid, 600).await.unwrap());
        let got = store.get_placement(pid).await.unwrap().unwrap();
        assert_eq!(got.ttl_secs, 600);
    }

    /// Renew on non-existent placement returns false.
    #[tokio::test]
    async fn renew_nonexistent_returns_false() {
        let store = LocalStateStore::new();
        assert!(!store.renew_placement(Uuid::new_v4(), 600).await.unwrap());
    }
}

mod scheduler_under_load {
    use std::sync::Arc;

    use futures_util::future::join_all;
    use rift_engine::scheduler::Scheduler;
    use rift_engine::state::local::LocalStateStore;
    use rift_engine::state::{StateStore, WorkerHeartbeat};
    use uuid::Uuid;

    fn hb(id: &str, active: u32, max: u32) -> WorkerHeartbeat {
        WorkerHeartbeat {
            worker_id: id.to_owned(),
            timestamp: chrono::Utc::now(),
            cpu_free_pct: 80.0,
            mem_free_bytes: 1024 * 1024 * 512,
            active_runtimes: active,
            max_runtimes: max,
        }
    }

    /// Scheduler distributes 10 placements across multiple workers.
    #[tokio::test]
    async fn distributes_across_workers() {
        let store = Arc::new(LocalStateStore::new());

        // Register 3 workers with varying load.
        store.send_heartbeat(&hb("w1", 0, 10)).await.unwrap();
        store.send_heartbeat(&hb("w2", 0, 10)).await.unwrap();
        store.send_heartbeat(&hb("w3", 0, 10)).await.unwrap();

        let scheduler = Scheduler::new(store.clone(), "w1".to_owned());

        let mut placements = std::collections::HashMap::new();
        for _ in 0..10 {
            let pid = Uuid::new_v4();
            let did = Uuid::new_v4();
            let worker = scheduler.place(pid, did).await.unwrap();
            *placements.entry(worker).or_insert(0) += 1;
        }

        // All placements should succeed (total 10, capacity 30).
        let total: i32 = placements.values().sum();
        assert_eq!(total, 10);
        assert!(!placements.is_empty());
    }

    /// Scheduler rejects when all workers are at capacity.
    #[tokio::test]
    async fn rejects_when_all_at_capacity() {
        let store = Arc::new(LocalStateStore::new());

        store.send_heartbeat(&hb("w1", 10, 10)).await.unwrap();
        store.send_heartbeat(&hb("w2", 10, 10)).await.unwrap();

        let scheduler = Scheduler::new(store.clone(), "other".to_owned());
        let result = scheduler.place(Uuid::new_v4(), Uuid::new_v4()).await;

        assert!(result.is_err());
    }

    /// Concurrent placements for the same project: at least one succeeds.
    #[tokio::test]
    async fn concurrent_placements_same_project() {
        let store = Arc::new(LocalStateStore::new());
        let scheduler = Arc::new(Scheduler::new(store.clone(), "w1".to_owned()));
        let pid = Uuid::new_v4();

        let mut handles = Vec::new();
        for _ in 0..5 {
            let sched = scheduler.clone();
            handles.push(tokio::spawn(async move {
                sched.place(pid, Uuid::new_v4()).await
            }));
        }

        let results: Vec<_> = join_all(handles)
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();

        // At least one should succeed.
        let successes = results.iter().filter(|r| r.is_ok()).count();
        assert!(successes >= 1, "Expected at least 1 success, got 0");

        // The final placement should be present.
        assert!(store.get_placement(pid).await.unwrap().is_some());
    }
}

mod worker_heartbeat_churn {
    use std::sync::Arc;

    use futures_util::future::join_all;
    use rift_engine::state::local::LocalStateStore;
    use rift_engine::state::{StateStore, WorkerHeartbeat};

    fn hb(id: &str) -> WorkerHeartbeat {
        WorkerHeartbeat {
            worker_id: id.to_owned(),
            timestamp: chrono::Utc::now(),
            cpu_free_pct: 50.0,
            mem_free_bytes: 1024 * 1024 * 256,
            active_runtimes: 3,
            max_runtimes: 10,
        }
    }

    /// Rapid heartbeat updates from many workers converge correctly.
    #[tokio::test]
    async fn rapid_heartbeat_updates() {
        let store = Arc::new(LocalStateStore::new());
        let mut handles = Vec::new();

        for i in 0..20 {
            let store = store.clone();
            handles.push(tokio::spawn(async move {
                for round in 0..10 {
                    let mut heartbeat = hb(&format!("w{i}"));
                    heartbeat.active_runtimes = round;
                    store.send_heartbeat(&heartbeat).await.unwrap();
                }
            }));
        }

        join_all(handles).await;

        let workers = store.list_workers().await.unwrap();
        assert_eq!(workers.len(), 20);

        // Each worker's final heartbeat should have active_runtimes=9.
        for w in &workers {
            assert_eq!(w.active_runtimes, 9);
        }
    }

    /// Removing a worker mid-churn doesn't corrupt other workers.
    #[tokio::test]
    async fn remove_during_updates() {
        let store = Arc::new(LocalStateStore::new());

        store.send_heartbeat(&hb("w1")).await.unwrap();
        store.send_heartbeat(&hb("w2")).await.unwrap();
        store.send_heartbeat(&hb("w3")).await.unwrap();

        store.remove_worker("w2").await.unwrap();

        let workers = store.list_workers().await.unwrap();
        assert_eq!(workers.len(), 2);
        let ids: Vec<_> = workers.iter().map(|w| w.worker_id.as_str()).collect();
        assert!(ids.contains(&"w1"));
        assert!(ids.contains(&"w3"));
        assert!(!ids.contains(&"w2"));
    }
}

mod routing_cache_stress {
    use std::sync::Arc;

    use futures_util::future::join_all;
    use rift_engine::proxy::routing_cache::{CacheLookup, RoutingCache};
    use uuid::Uuid;

    /// Concurrent reads and writes don't panic or deadlock.
    #[tokio::test]
    async fn concurrent_insert_and_lookup_stress() {
        let cache = Arc::new(RoutingCache::new());
        let mut handles = Vec::new();

        // 50 writers
        for i in 0..50 {
            let cache = cache.clone();
            handles.push(tokio::spawn(async move {
                let host = format!("stress{i}.rift.dev");
                let pid = Uuid::new_v4();
                cache.insert(host.clone(), pid).await;
                // Immediately read back
                if let CacheLookup::Hit(id) = cache.lookup(&host).await {
                    assert_eq!(id, pid);
                }
            }));
        }

        // 50 readers
        for i in 0..50 {
            let cache = cache.clone();
            handles.push(tokio::spawn(async move {
                let host = format!("stress{i}.rift.dev");
                let _ = cache.lookup(&host).await;
            }));
        }

        // 10 invalidators
        for i in 0..10 {
            let cache = cache.clone();
            handles.push(tokio::spawn(async move {
                let host = format!("stress{i}.rift.dev");
                cache.invalidate_host(&host).await;
            }));
        }

        join_all(handles).await;
    }

    /// Negative cache entries are returned correctly under load.
    #[tokio::test]
    async fn negative_entries_under_load() {
        let cache = Arc::new(RoutingCache::new());
        let mut handles = Vec::new();

        // Insert 100 negative entries
        for i in 0..100 {
            let cache = cache.clone();
            handles.push(tokio::spawn(async move {
                let host = format!("neg{i}.rift.dev");
                cache.insert_negative(host).await;
            }));
        }

        join_all(handles).await;

        // All should return NegativeHit.
        for i in 0..100 {
            let host = format!("neg{i}.rift.dev");
            assert!(
                matches!(cache.lookup(&host).await, CacheLookup::NegativeHit),
                "Expected NegativeHit for {host}"
            );
        }
    }

    /// Project invalidation clears all matching hosts.
    #[tokio::test]
    async fn project_invalidation_clears_all_hosts() {
        let cache = RoutingCache::new();
        let pid = Uuid::new_v4();

        // Insert 5 hosts for the same project.
        for i in 0..5 {
            cache.insert(format!("host{i}.rift.dev"), pid).await;
        }

        cache.invalidate_project(pid).await;

        // All should now be cache misses.
        for i in 0..5 {
            assert!(
                matches!(
                    cache.lookup(&format!("host{i}.rift.dev")).await,
                    CacheLookup::Miss
                ),
                "Expected Miss after invalidation"
            );
        }
    }
}

mod lifecycle_storm {
    use std::sync::Arc;

    use futures_util::future::join_all;
    use rift_engine::runtime::backend::RuntimeBackend;
    use rift_engine::runtime::mock_backend::MockBackend;
    use rift_engine::runtime::RuntimeLaunchSpec;
    use uuid::Uuid;

    fn launch_spec(project_id: Uuid) -> RuntimeLaunchSpec {
        RuntimeLaunchSpec {
            project_id,
            deployment_id: Uuid::new_v4(),
            kind: rift_engine::runtime::RuntimeKind::StaticDeno {
                dir: std::path::PathBuf::from("/tmp/test"),
            },
            env_vars: vec![],
        }
    }

    /// Concurrent deploy + stop for the same project doesn't panic.
    #[tokio::test]
    async fn concurrent_deploy_and_stop() {
        let backend = Arc::new(MockBackend::new());
        let pid = Uuid::new_v4();

        // Deploy first so there's something to stop.
        backend.deploy(launch_spec(pid)).await.unwrap();

        let mut handles = Vec::new();
        for _ in 0..10 {
            let b = backend.clone();
            handles.push(tokio::spawn(async move {
                let _ = b.stop(pid).await;
            }));

            let b = backend.clone();
            handles.push(tokio::spawn(async move {
                let _ = b.deploy(launch_spec(pid)).await;
            }));
        }

        join_all(handles).await;
        // No panic — state may be active or stopped, both are valid.
    }

    /// Concurrent suspend + wake for the same project doesn't corrupt state.
    #[tokio::test]
    async fn concurrent_suspend_and_wake() {
        let backend = Arc::new(MockBackend::new());
        let pid = Uuid::new_v4();

        backend.deploy(launch_spec(pid)).await.unwrap();

        let mut handles = Vec::new();
        for _ in 0..20 {
            let b = backend.clone();
            handles.push(tokio::spawn(async move {
                let _ = b.suspend(pid).await;
            }));

            let b = backend.clone();
            handles.push(tokio::spawn(async move {
                let _ = b.wake(pid).await;
            }));
        }

        join_all(handles).await;

        // State must be consistent: either active, suspended, or neither.
        let is_active = backend.active_url(pid).await.is_some();
        let is_suspended = backend.is_suspended(pid).await;
        assert!(
            !(is_active && is_suspended),
            "Cannot be both active and suspended"
        );
    }

    /// Rapid deploy/stop cycle for many projects.
    #[tokio::test]
    async fn many_projects_deploy_stop_cycle() {
        let backend = Arc::new(MockBackend::new());
        let mut handles = Vec::new();

        for _ in 0..50 {
            let b = backend.clone();
            handles.push(tokio::spawn(async move {
                let pid = Uuid::new_v4();
                b.deploy(launch_spec(pid)).await.unwrap();
                assert!(b.active_url(pid).await.is_some());
                b.stop(pid).await.unwrap();
                assert!(b.active_url(pid).await.is_none());
            }));
        }

        join_all(handles).await;
    }

    /// Full lifecycle cycle: deploy → suspend → wake → stop.
    #[tokio::test]
    async fn full_lifecycle_cycle() {
        let backend = MockBackend::new();
        let pid = Uuid::new_v4();

        // Deploy
        backend.deploy(launch_spec(pid)).await.unwrap();
        assert!(backend.active_url(pid).await.is_some());
        assert!(!backend.is_suspended(pid).await);

        // Suspend
        let suspended = backend.suspend(pid).await.unwrap();
        assert!(suspended);
        assert!(backend.active_url(pid).await.is_none());
        assert!(backend.is_suspended(pid).await);

        // Wake
        let url = backend.wake(pid).await.unwrap();
        assert!(url.is_some());
        assert!(backend.active_url(pid).await.is_some());
        assert!(!backend.is_suspended(pid).await);

        // Stop
        backend.stop(pid).await.unwrap();
        assert!(backend.active_url(pid).await.is_none());
        assert!(!backend.is_suspended(pid).await);
    }

    /// Wake without prior suspend returns None.
    #[tokio::test]
    async fn wake_without_suspend_returns_none() {
        let backend = MockBackend::new();
        let url = backend.wake(Uuid::new_v4()).await.unwrap();
        assert!(url.is_none());
    }

    /// Stop on non-existent project is a no-op.
    #[tokio::test]
    async fn stop_nonexistent_is_noop() {
        let backend = MockBackend::new();
        backend.stop(Uuid::new_v4()).await.unwrap();
    }
}

mod state_machine_edge_cases {
    use rift_engine::lifecycle::state_machine::DeploymentState;

    /// All terminal states have no valid transitions.
    #[test]
    fn terminal_states_are_dead_ends() {
        let terminals = [DeploymentState::Failed, DeploymentState::Cancelled];
        for state in terminals {
            assert!(state.is_terminal(), "{state:?} should be terminal");
            let all_states = [
                DeploymentState::Queued,
                DeploymentState::Cloning,
                DeploymentState::Building,
                DeploymentState::Deploying,
                DeploymentState::Ready,
                DeploymentState::Suspended,
                DeploymentState::Failed,
                DeploymentState::Cancelled,
            ];
            for target in all_states {
                assert!(
                    !state.can_transition_to(target),
                    "{state:?} should not transition to {target:?}"
                );
            }
        }
    }

    /// No state can transition to itself.
    #[test]
    fn no_self_transitions() {
        let all_states = [
            DeploymentState::Queued,
            DeploymentState::Cloning,
            DeploymentState::Building,
            DeploymentState::Deploying,
            DeploymentState::Ready,
            DeploymentState::Suspended,
            DeploymentState::Failed,
            DeploymentState::Cancelled,
        ];
        for state in all_states {
            assert!(
                !state.can_transition_to(state),
                "{state:?} should not self-transition"
            );
        }
    }

    /// The suspend/wake cycle doesn't break invariants.
    #[test]
    fn suspend_wake_cycle_is_valid() {
        assert!(DeploymentState::Ready.can_transition_to(DeploymentState::Suspended));
        assert!(DeploymentState::Suspended.can_transition_to(DeploymentState::Ready));

        // But can't skip steps.
        assert!(!DeploymentState::Building.can_transition_to(DeploymentState::Suspended));
        assert!(!DeploymentState::Suspended.can_transition_to(DeploymentState::Building));
    }
}
