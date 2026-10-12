use serde::{Deserialize, Serialize};
use std::sync::RwLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ServerState {
    Starting,
    Running,
    Error,
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeStatus {
    pub state: ServerState,
    pub endpoint: String,
    pub error: Option<String>,
}

impl RuntimeStatus {
    pub fn starting(endpoint: impl Into<String>) -> Self {
        Self {
            state: ServerState::Starting,
            endpoint: endpoint.into(),
            error: None,
        }
    }

    pub fn running(endpoint: impl Into<String>) -> Self {
        Self {
            state: ServerState::Running,
            endpoint: endpoint.into(),
            error: None,
        }
    }

    pub fn error(endpoint: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            state: ServerState::Error,
            endpoint: endpoint.into(),
            error: Some(message.into()),
        }
    }

    pub fn stopped(endpoint: impl Into<String>) -> Self {
        Self {
            state: ServerState::Stopped,
            endpoint: endpoint.into(),
            error: None,
        }
    }
}

pub struct AppState {
    status: RwLock<RuntimeStatus>,
}

impl AppState {
    pub fn new(initial_status: RuntimeStatus) -> Self {
        Self {
            status: RwLock::new(initial_status),
        }
    }

    pub fn get_status(&self) -> RuntimeStatus {
        self.status
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn set_status(&self, new_status: RuntimeStatus) {
        let mut lock = self
            .status
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *lock = new_status;
    }
}
