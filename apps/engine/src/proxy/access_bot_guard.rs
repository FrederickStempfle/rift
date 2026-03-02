use std::{
    collections::{HashMap, HashSet, VecDeque},
    hash::{Hash, Hasher},
    net::IpAddr,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use crate::config::Config;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MitigationAction {
    Challenge,
    Block,
}

#[derive(Clone, Debug)]
pub struct MitigationDecision {
    pub action: MitigationAction,
    pub retry_after_secs: u64,
    pub reason: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AccessBotMode {
    Off,
    Challenge,
    Block,
}

#[derive(Clone, Debug)]
struct GuardConfig {
    mode: AccessBotMode,
    window: Duration,
    burst_threshold: u32,
    scan_unique_paths: u32,
    scan_404_threshold: u32,
    mitigation_duration: Duration,
}

#[derive(Clone, Debug)]
pub struct AccessBotGuard {
    inner: Arc<GuardInner>,
}

#[derive(Debug)]
struct GuardInner {
    config: GuardConfig,
    windows: Mutex<HashMap<IpAddr, IpWindow>>,
    mitigations: Mutex<HashMap<IpAddr, MitigationState>>,
}

#[derive(Debug)]
struct IpWindow {
    events: VecDeque<RequestSample>,
}

#[derive(Debug)]
struct RequestSample {
    at: Instant,
    path_hash: u64,
    status: u16,
}

#[derive(Debug)]
struct MitigationState {
    until: Instant,
    reason: String,
}

impl AccessBotGuard {
    pub fn from_config(config: &Config) -> Self {
        let mode = parse_mode(&config.access_bot_mode);
        let cfg = GuardConfig {
            mode,
            window: Duration::from_secs(config.access_bot_window_secs.max(1)),
            burst_threshold: config.access_bot_burst_threshold.max(1),
            scan_unique_paths: config.access_bot_scan_unique_paths.max(1),
            scan_404_threshold: config.access_bot_scan_404_threshold.max(1),
            mitigation_duration: Duration::from_secs(config.access_bot_mitigation_secs.max(1)),
        };
        Self {
            inner: Arc::new(GuardInner {
                config: cfg,
                windows: Mutex::new(HashMap::new()),
                mitigations: Mutex::new(HashMap::new()),
            }),
        }
    }

    pub fn observe(&self, client_ip: IpAddr, path: &str, status: u16) {
        if self.inner.config.mode == AccessBotMode::Off {
            return;
        }

        let now = Instant::now();
        let mut windows = self
            .inner
            .windows
            .lock()
            .expect("access bot windows poisoned");
        let window = windows.entry(client_ip).or_insert_with(|| IpWindow {
            events: VecDeque::new(),
        });
        window.prune(now, self.inner.config.window);
        window.events.push_back(RequestSample {
            at: now,
            path_hash: hash_path(path),
            status,
        });

        if window.events.len() > 8_192 {
            let drop_count = window.events.len() - 8_192;
            for _ in 0..drop_count {
                window.events.pop_front();
            }
        }

        let req_count = window.events.len() as u32;
        let unique_paths = window
            .events
            .iter()
            .map(|entry| entry.path_hash)
            .collect::<HashSet<_>>()
            .len() as u32;
        let not_found = window
            .events
            .iter()
            .filter(|entry| entry.status == 404)
            .count() as u32;

        let burst_triggered = req_count >= self.inner.config.burst_threshold;
        let scanner_triggered = unique_paths >= self.inner.config.scan_unique_paths
            && not_found >= self.inner.config.scan_404_threshold;
        if !(burst_triggered || scanner_triggered) {
            return;
        }

        let reason = if burst_triggered {
            format!(
                "high request burst detected ({} req in {}s)",
                req_count,
                self.inner.config.window.as_secs()
            )
        } else {
            format!(
                "scanner pattern detected ({} unique paths, {} 404s in {}s)",
                unique_paths,
                not_found,
                self.inner.config.window.as_secs()
            )
        };
        drop(windows);
        self.insert_mitigation(client_ip, now, reason);
    }

    pub fn evaluate_with_path(&self, client_ip: IpAddr, path: &str) -> Option<MitigationDecision> {
        if self.inner.config.mode == AccessBotMode::Off {
            return None;
        }

        let now = Instant::now();
        if let Some(decision) = self.evaluate_active(client_ip, now) {
            return Some(decision);
        }

        if let Some(signature) = scanner_signature(path) {
            let reason = format!("scanner signature matched ({signature})");
            self.insert_mitigation(client_ip, now, reason.clone());
            return Some(MitigationDecision {
                action: mode_to_action(self.inner.config.mode)?,
                retry_after_secs: self.inner.config.mitigation_duration.as_secs().max(1),
                reason,
            });
        }

        None
    }

    pub fn evaluate(&self, client_ip: IpAddr) -> Option<MitigationDecision> {
        if self.inner.config.mode == AccessBotMode::Off {
            return None;
        }

        let now = Instant::now();
        self.evaluate_active(client_ip, now)
    }

    fn evaluate_active(&self, client_ip: IpAddr, now: Instant) -> Option<MitigationDecision> {
        let mut mitigations = self
            .inner
            .mitigations
            .lock()
            .expect("access bot mitigations poisoned");
        let state = mitigations.get(&client_ip)?;
        if now >= state.until {
            mitigations.remove(&client_ip);
            return None;
        }
        Some(MitigationDecision {
            action: mode_to_action(self.inner.config.mode)?,
            retry_after_secs: state.until.saturating_duration_since(now).as_secs().max(1),
            reason: state.reason.clone(),
        })
    }

    fn insert_mitigation(&self, client_ip: IpAddr, now: Instant, reason: String) {
        let mut mitigations = self
            .inner
            .mitigations
            .lock()
            .expect("access bot mitigations poisoned");
        mitigations.insert(
            client_ip,
            MitigationState {
                until: now + self.inner.config.mitigation_duration,
                reason,
            },
        );
    }
}

impl IpWindow {
    fn prune(&mut self, now: Instant, window: Duration) {
        while let Some(front) = self.events.front() {
            if now.saturating_duration_since(front.at) <= window {
                break;
            }
            self.events.pop_front();
        }
    }
}

fn parse_mode(raw: &str) -> AccessBotMode {
    match raw.trim().to_ascii_lowercase().as_str() {
        "challenge" => AccessBotMode::Challenge,
        "block" => AccessBotMode::Block,
        _ => AccessBotMode::Off,
    }
}

fn mode_to_action(mode: AccessBotMode) -> Option<MitigationAction> {
    match mode {
        AccessBotMode::Off => None,
        AccessBotMode::Challenge => Some(MitigationAction::Challenge),
        AccessBotMode::Block => Some(MitigationAction::Block),
    }
}

fn scanner_signature(path: &str) -> Option<&'static str> {
    let path = path
        .split('?')
        .next()
        .unwrap_or(path)
        .trim()
        .to_ascii_lowercase();
    if path.is_empty() {
        return None;
    }

    if path.contains("../")
        || path.contains("%2e%2e")
        || path.contains("..%2f")
        || path.contains("%252e%252e")
        || path.contains("%00")
    {
        return Some("path traversal probe");
    }

    if path.starts_with("/.env")
        || path.starts_with("/.git")
        || path.starts_with("/.aws")
        || path.starts_with("/phpmyadmin")
        || path.starts_with("/wp-admin")
        || path.starts_with("/wp-includes")
        || path.starts_with("/vendor/phpunit")
        || path.starts_with("/cgi-bin/")
    {
        return Some("sensitive path probe");
    }

    if path == "/wp-login.php"
        || path == "/xmlrpc.php"
        || path == "/boaform/admin/formlogin"
        || path == "/.git/config"
        || path == "/.env"
    {
        return Some("known exploit probe");
    }

    None
}

fn hash_path(path: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_guard(mode: AccessBotMode) -> AccessBotGuard {
        AccessBotGuard {
            inner: Arc::new(GuardInner {
                config: GuardConfig {
                    mode,
                    window: Duration::from_secs(30),
                    burst_threshold: 100,
                    scan_unique_paths: 30,
                    scan_404_threshold: 20,
                    mitigation_duration: Duration::from_secs(120),
                },
                windows: Mutex::new(HashMap::new()),
                mitigations: Mutex::new(HashMap::new()),
            }),
        }
    }

    #[test]
    fn scanner_signature_detects_known_probe() {
        assert_eq!(
            scanner_signature("/wp-login.php"),
            Some("known exploit probe")
        );
        assert_eq!(scanner_signature("/.env"), Some("sensitive path probe"));
        assert_eq!(
            scanner_signature("/../../etc/passwd"),
            Some("path traversal probe")
        );
        assert_eq!(scanner_signature("/pricing"), None);
    }

    #[test]
    fn evaluate_with_path_issues_block_and_persists_mitigation() {
        let guard = test_guard(AccessBotMode::Block);
        let ip: IpAddr = "203.0.113.42".parse().unwrap();

        let decision = guard.evaluate_with_path(ip, "/wp-login.php").unwrap();
        assert_eq!(decision.action, MitigationAction::Block);
        assert!(decision.reason.contains("scanner signature matched"));
        assert!(decision.retry_after_secs >= 1);

        let follow_up = guard.evaluate(ip).unwrap();
        assert_eq!(follow_up.action, MitigationAction::Block);
    }

    #[test]
    fn evaluate_with_path_ignores_normal_routes() {
        let guard = test_guard(AccessBotMode::Challenge);
        let ip: IpAddr = "203.0.113.15".parse().unwrap();
        assert!(guard
            .evaluate_with_path(ip, "/docs/getting-started")
            .is_none());
    }
}
