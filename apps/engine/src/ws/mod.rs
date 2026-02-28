pub mod broadcast;

#[derive(Clone, Debug, Default)]
pub struct LogBroadcaster;

impl LogBroadcaster {
    pub fn new() -> Self {
        Self
    }
}
