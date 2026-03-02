//! Deployment lifecycle state machine and CAS transition functions.
//!
//! The state machine enforces explicit, valid transitions between
//! deployment states. CAS (compare-and-set) transitions prevent
//! concurrent mutations from corrupting state.

pub mod operations;
pub mod state_machine;
pub mod transition;
