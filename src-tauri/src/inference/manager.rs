use crate::inference::InferenceError;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Instant;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

pub const DEFAULT_MAX_CONCURRENT_INFERENCE: usize = 2;

#[derive(Debug, Clone)]
pub struct ActiveRequestInfo {
    pub request_id: String,
    pub model_id: String,
    pub window_label: Option<String>,
    pub cancellation_token: CancellationToken,
    pub started_at: Instant,
}

pub struct TerminalGuard {
    emitted: AtomicBool,
}

impl Default for TerminalGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl TerminalGuard {
    pub fn new() -> Self {
        Self {
            emitted: AtomicBool::new(false),
        }
    }

    pub fn try_claim(&self) -> bool {
        self.emitted
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    pub fn has_emitted(&self) -> bool {
        self.emitted.load(Ordering::SeqCst)
    }
}

pub struct InferenceManager {
    active_requests: RwLock<HashMap<String, ActiveRequestInfo>>,
    semaphore: Arc<Semaphore>,
}

impl Default for InferenceManager {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_CONCURRENT_INFERENCE)
    }
}

impl InferenceManager {
    pub fn new(max_concurrent: usize) -> Self {
        Self {
            active_requests: RwLock::new(HashMap::new()),
            semaphore: Arc::new(Semaphore::new(max_concurrent)),
        }
    }

    pub fn register_request(
        &self,
        request_id: String,
        model_id: String,
        window_label: Option<String>,
    ) -> Result<CancellationToken, InferenceError> {
        let mut map = self
            .active_requests
            .write()
            .unwrap_or_else(|p| p.into_inner());

        if map.contains_key(&request_id) {
            return Err(InferenceError::InvalidRequest(format!(
                "Inference request '{}' is already in progress",
                request_id
            )));
        }

        let cancellation_token = CancellationToken::new();
        map.insert(
            request_id.clone(),
            ActiveRequestInfo {
                request_id,
                model_id,
                window_label,
                cancellation_token: cancellation_token.clone(),
                started_at: Instant::now(),
            },
        );

        Ok(cancellation_token)
    }

    pub fn cancel_request(&self, request_id: &str, caller_window: Option<&str>) -> bool {
        let map = self
            .active_requests
            .read()
            .unwrap_or_else(|p| p.into_inner());

        if let Some(info) = map.get(request_id) {
            // If caller specifies a window, and the request is bound to a window, verify match
            if matches!(
                (caller_window, &info.window_label),
                (Some(caller), Some(target_window)) if caller != target_window
            ) {
                return false;
            }
            info.cancellation_token.cancel();
            true
        } else {
            false
        }
    }

    pub fn unregister_request(&self, request_id: &str) {
        let mut map = self
            .active_requests
            .write()
            .unwrap_or_else(|p| p.into_inner());
        map.remove(request_id);
    }

    pub async fn acquire_permit_cancellable(
        &self,
        token: &CancellationToken,
    ) -> Result<OwnedSemaphorePermit, InferenceError> {
        tokio::select! {
            _ = token.cancelled() => Err(InferenceError::Cancelled),
            permit_res = self.semaphore.clone().acquire_owned() => {
                permit_res.map_err(|_| InferenceError::InvalidRequest("Capacity manager shut down".to_string()))
            }
        }
    }

    pub async fn acquire_permit(&self) -> Result<OwnedSemaphorePermit, InferenceError> {
        self.acquire_permit_cancellable(&CancellationToken::new())
            .await
    }

    pub fn active_count(&self) -> usize {
        let map = self
            .active_requests
            .read()
            .unwrap_or_else(|p| p.into_inner());
        map.len()
    }
}
