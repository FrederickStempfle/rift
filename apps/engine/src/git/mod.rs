pub mod clone;
pub mod webhook;

#[derive(Clone, Debug, Default)]
pub struct GitManager;

impl GitManager {
    pub fn new() -> Self {
        Self
    }
}
