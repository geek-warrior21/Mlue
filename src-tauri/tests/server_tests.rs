use http_body_util::BodyExt;
use mlue_lib::api::health::{APP_VERSION, HealthResponse, SERVICE_NAME, health_handler};
use mlue_lib::runtime::server::{
    DEFAULT_HOST, DEFAULT_PORT, build_app_router, default_endpoint, run_server,
};
use mlue_lib::runtime::state::{AppState, RuntimeStatus, ServerState};
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tower::ServiceExt;

#[tokio::test]
async fn test_health_handler_direct() {
    let response = health_handler().await;
    assert_eq!(response.status, "ok");
    assert_eq!(response.service, SERVICE_NAME);
    assert_eq!(response.version, APP_VERSION);
    assert_eq!(response.version, "0.1.0");
}

#[tokio::test]
async fn test_health_endpoint_via_router() {
    let router = build_app_router();
    let request = axum::http::Request::builder()
        .method(axum::http::Method::GET)
        .uri("/health")
        .body(axum::body::Body::empty())
        .expect("failed to construct request");

    let response = router
        .oneshot(request)
        .await
        .expect("failed to call router");
    assert_eq!(response.status(), axum::http::StatusCode::OK);

    let body_bytes = response
        .into_body()
        .collect()
        .await
        .expect("failed to read body")
        .to_bytes();

    let health: HealthResponse = serde_json::from_slice(&body_bytes).expect("failed to parse json");
    assert_eq!(health.status, "ok");
    assert_eq!(health.service, "mlue");
    assert_eq!(health.version, "0.1.0");
}

#[tokio::test]
async fn test_server_config_binds_only_to_loopback() {
    assert_eq!(DEFAULT_HOST, "127.0.0.1");
    assert_eq!(DEFAULT_PORT, 4782);
    assert_eq!(default_endpoint(), "http://127.0.0.1:4782");

    let app_state = Arc::new(AppState::new(RuntimeStatus::starting(default_endpoint())));
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);

    // Attempting to bind to 0.0.0.0 must be strictly rejected
    let result = run_server("0.0.0.0", 4782, app_state.clone(), shutdown_rx).await;
    assert!(result.is_err(), "Binding to 0.0.0.0 must be prohibited");
    assert_eq!(app_state.get_status().state, ServerState::Error);
    let error_msg = app_state.get_status().error.unwrap();
    assert!(error_msg.contains("non-loopback host"));
}

#[tokio::test]
async fn test_port_collision_produces_controlled_startup_error() {
    // Bind a listener on an ephemeral loopback port to simulate port collision
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("failed to bind ephemeral port");
    let occupied_port = listener.local_addr().unwrap().port();

    let endpoint = format!("http://127.0.0.1:{}", occupied_port);
    let app_state = Arc::new(AppState::new(RuntimeStatus::starting(&endpoint)));
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);

    // Attempt to start embedded server on the already-occupied port
    let result = run_server("127.0.0.1", occupied_port, app_state.clone(), shutdown_rx).await;

    assert!(result.is_err(), "Server should fail to bind occupied port");
    let status = app_state.get_status();
    assert_eq!(
        status.state,
        ServerState::Error,
        "AppState must reflect Error state"
    );
    assert!(
        status.error.is_some(),
        "AppState must include diagnostic error message"
    );
    assert!(
        status.error.unwrap().contains("Failed to bind"),
        "Error message should mention bind failure"
    );

    // Clean up
    drop(listener);
}

#[tokio::test]
async fn test_runtime_state_transitions() {
    let endpoint = "http://127.0.0.1:4782";
    let state = AppState::new(RuntimeStatus::starting(endpoint));

    // Verify Starting state
    let s1 = state.get_status();
    assert_eq!(s1.state, ServerState::Starting);
    assert_eq!(s1.endpoint, endpoint);
    assert!(s1.error.is_none());

    // Transition to Running
    state.set_status(RuntimeStatus::running(endpoint));
    let s2 = state.get_status();
    assert_eq!(s2.state, ServerState::Running);
    assert_eq!(s2.endpoint, endpoint);
    assert!(s2.error.is_none());

    // Transition to Error
    state.set_status(RuntimeStatus::error(endpoint, "Simulated port conflict"));
    let s3 = state.get_status();
    assert_eq!(s3.state, ServerState::Error);
    assert_eq!(s3.endpoint, endpoint);
    assert_eq!(s3.error.as_deref(), Some("Simulated port conflict"));

    // Transition to Stopped
    state.set_status(RuntimeStatus::stopped(endpoint));
    let s4 = state.get_status();
    assert_eq!(s4.state, ServerState::Stopped);
    assert_eq!(s4.endpoint, endpoint);
    assert!(s4.error.is_none());
}

#[tokio::test]
async fn test_live_server_lifecycle_and_graceful_shutdown() {
    // Find an available loopback port
    let probe = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = probe.local_addr().unwrap().port();
    drop(probe); // Free the port immediately for our server

    let endpoint = format!("http://127.0.0.1:{}", port);
    let app_state = Arc::new(AppState::new(RuntimeStatus::starting(&endpoint)));
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let state_clone = app_state.clone();
    let server_task =
        tokio::spawn(async move { run_server("127.0.0.1", port, state_clone, shutdown_rx).await });

    // Wait for server to enter Running state
    let mut running = false;
    for _ in 0..50 {
        if app_state.get_status().state == ServerState::Running {
            running = true;
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;
    }
    assert!(running, "Server failed to transition to Running state");

    // Perform live HTTP request to /health
    let client = reqwest::Client::new();
    let res = client
        .get(format!("{}/health", endpoint))
        .send()
        .await
        .expect("failed to send request");

    assert_eq!(res.status(), reqwest::StatusCode::OK);
    let json: HealthResponse = res.json().await.expect("failed to parse json");
    assert_eq!(json.status, "ok");
    assert_eq!(json.service, "mlue");
    assert_eq!(json.version, "0.1.0");

    // Signal graceful shutdown
    shutdown_tx.send(true).expect("failed to send shutdown");

    let server_result = server_task.await.expect("server task panicked");
    assert!(server_result.is_ok(), "server exited with error");
    assert_eq!(app_state.get_status().state, ServerState::Stopped);
}
