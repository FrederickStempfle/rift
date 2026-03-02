//! Deployment state enum and valid transition rules.
//!
//! ```text
//! Queued → Cloning, Failed, Cancelled
//! Cloning → Building, Failed, Cancelled
//! Building → Deploying, Failed, Cancelled
//! Deploying → Ready, Failed
//! Ready → Suspended, Cancelled, Failed
//! Suspended → Ready, Cancelled, Failed
//! Failed, Cancelled → (terminal)
//! ```

use std::fmt;

/// All possible deployment states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeploymentState {
    Queued,
    Cloning,
    Building,
    Deploying,
    Ready,
    Suspended,
    Failed,
    Cancelled,
}

impl DeploymentState {
    /// States reachable from this state.
    pub fn valid_transitions(self) -> &'static [DeploymentState] {
        use DeploymentState::*;
        match self {
            Queued => &[Cloning, Failed, Cancelled],
            Cloning => &[Building, Failed, Cancelled],
            Building => &[Deploying, Failed, Cancelled],
            Deploying => &[Ready, Failed],
            Ready => &[Suspended, Cancelled, Failed],
            Suspended => &[Ready, Cancelled, Failed],
            Failed | Cancelled => &[],
        }
    }

    /// Whether a transition to `target` is allowed.
    pub fn can_transition_to(self, target: DeploymentState) -> bool {
        self.valid_transitions().contains(&target)
    }

    /// Whether this state is terminal (no further transitions).
    pub fn is_terminal(self) -> bool {
        self.valid_transitions().is_empty()
    }

    /// Parse from the database string representation.
    pub fn parse(s: &str) -> Option<DeploymentState> {
        match s {
            "queued" => Some(DeploymentState::Queued),
            "cloning" => Some(DeploymentState::Cloning),
            "building" => Some(DeploymentState::Building),
            "deploying" => Some(DeploymentState::Deploying),
            "ready" => Some(DeploymentState::Ready),
            "suspended" => Some(DeploymentState::Suspended),
            "failed" => Some(DeploymentState::Failed),
            "cancelled" => Some(DeploymentState::Cancelled),
            _ => None,
        }
    }

    /// Database string representation.
    pub fn as_str(self) -> &'static str {
        match self {
            DeploymentState::Queued => "queued",
            DeploymentState::Cloning => "cloning",
            DeploymentState::Building => "building",
            DeploymentState::Deploying => "deploying",
            DeploymentState::Ready => "ready",
            DeploymentState::Suspended => "suspended",
            DeploymentState::Failed => "failed",
            DeploymentState::Cancelled => "cancelled",
        }
    }
}

impl fmt::Display for DeploymentState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_STATES: [DeploymentState; 8] = [
        DeploymentState::Queued,
        DeploymentState::Cloning,
        DeploymentState::Building,
        DeploymentState::Deploying,
        DeploymentState::Ready,
        DeploymentState::Suspended,
        DeploymentState::Failed,
        DeploymentState::Cancelled,
    ];

    #[test]
    fn queued_can_transition_to_cloning() {
        assert!(DeploymentState::Queued.can_transition_to(DeploymentState::Cloning));
    }

    #[test]
    fn queued_can_transition_to_failed() {
        assert!(DeploymentState::Queued.can_transition_to(DeploymentState::Failed));
    }

    #[test]
    fn queued_can_transition_to_cancelled() {
        assert!(DeploymentState::Queued.can_transition_to(DeploymentState::Cancelled));
    }

    #[test]
    fn queued_cannot_transition_to_ready() {
        assert!(!DeploymentState::Queued.can_transition_to(DeploymentState::Ready));
    }

    #[test]
    fn queued_cannot_transition_to_deploying() {
        assert!(!DeploymentState::Queued.can_transition_to(DeploymentState::Deploying));
    }

    #[test]
    fn cloning_can_transition_to_building() {
        assert!(DeploymentState::Cloning.can_transition_to(DeploymentState::Building));
    }

    #[test]
    fn building_can_transition_to_deploying() {
        assert!(DeploymentState::Building.can_transition_to(DeploymentState::Deploying));
    }

    #[test]
    fn deploying_can_transition_to_ready() {
        assert!(DeploymentState::Deploying.can_transition_to(DeploymentState::Ready));
    }

    #[test]
    fn deploying_can_transition_to_failed() {
        assert!(DeploymentState::Deploying.can_transition_to(DeploymentState::Failed));
    }

    #[test]
    fn deploying_cannot_transition_to_cancelled() {
        assert!(!DeploymentState::Deploying.can_transition_to(DeploymentState::Cancelled));
    }

    #[test]
    fn ready_can_transition_to_suspended() {
        assert!(DeploymentState::Ready.can_transition_to(DeploymentState::Suspended));
    }

    #[test]
    fn ready_can_transition_to_cancelled() {
        assert!(DeploymentState::Ready.can_transition_to(DeploymentState::Cancelled));
    }

    #[test]
    fn ready_is_not_terminal() {
        assert!(!DeploymentState::Ready.is_terminal());
    }

    #[test]
    fn suspended_can_transition_to_ready() {
        assert!(DeploymentState::Suspended.can_transition_to(DeploymentState::Ready));
    }

    #[test]
    fn suspended_can_transition_to_cancelled() {
        assert!(DeploymentState::Suspended.can_transition_to(DeploymentState::Cancelled));
    }

    #[test]
    fn suspended_can_transition_to_failed() {
        assert!(DeploymentState::Suspended.can_transition_to(DeploymentState::Failed));
    }

    #[test]
    fn suspended_cannot_transition_to_building() {
        assert!(!DeploymentState::Suspended.can_transition_to(DeploymentState::Building));
    }

    #[test]
    fn suspended_is_not_terminal() {
        assert!(!DeploymentState::Suspended.is_terminal());
    }

    #[test]
    fn failed_is_terminal() {
        assert!(DeploymentState::Failed.is_terminal());
    }

    #[test]
    fn cancelled_is_terminal() {
        assert!(DeploymentState::Cancelled.is_terminal());
    }

    #[test]
    fn queued_is_not_terminal() {
        assert!(!DeploymentState::Queued.is_terminal());
    }

    #[test]
    fn round_trip_all_states() {
        for state in ALL_STATES {
            let s = state.as_str();
            let parsed = DeploymentState::parse(s).expect("should parse");
            assert_eq!(parsed, state);
        }
    }

    #[test]
    fn from_str_unknown_returns_none() {
        assert!(DeploymentState::parse("bogus").is_none());
    }

    #[test]
    fn all_non_terminal_states_have_transitions() {
        for state in [
            DeploymentState::Queued,
            DeploymentState::Cloning,
            DeploymentState::Building,
            DeploymentState::Deploying,
            DeploymentState::Ready,
            DeploymentState::Suspended,
        ] {
            assert!(
                !state.valid_transitions().is_empty(),
                "{state} should have transitions"
            );
        }
    }

    #[test]
    fn no_state_can_transition_to_itself() {
        for state in ALL_STATES {
            assert!(
                !state.can_transition_to(state),
                "{state} should not be able to transition to itself"
            );
        }
    }
}
