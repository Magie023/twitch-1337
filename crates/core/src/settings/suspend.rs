//! Suspend command runtime settings.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SuspendSettings {
    pub default_duration_secs: u64,
}

impl Default for SuspendSettings {
    fn default() -> Self {
        Self {
            default_duration_secs: 600,
        }
    }
}
