use std::time::Duration;

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header::CONTENT_TYPE},
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;
use turbofieldfare_protocol::{GenerationEvent, GenerationRequest};

use super::{ApiConfig, EVENT_CHANNEL_CAPACITY, InferenceBackend, MockBackend, app};

fn chat_request(body: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_owned()))
        .expect("request")
}

async fn body_text(response: axum::response::Response) -> String {
    String::from_utf8(
        to_bytes(response.into_body(), 2 * 1024 * 1024)
            .await
            .expect("response body")
            .to_vec(),
    )
    .expect("UTF-8 response")
}

#[tokio::test]
async fn health_models_and_non_streaming_contract() {
    let (router, _) = app(ApiConfig {
        model_id: "test-model".into(),
        ..ApiConfig::default()
    });
    let health = router
        .clone()
        .oneshot(
            Request::get("/health")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("health response");
    assert_eq!(health.status(), StatusCode::OK);
    assert!(body_text(health).await.contains(r#""status":"ok""#));

    let models = router
        .clone()
        .oneshot(
            Request::get("/v1/models?client=test")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("models response");
    assert_eq!(models.status(), StatusCode::OK);
    assert!(body_text(models).await.contains("test-model"));

    let completion = router
        .oneshot(chat_request(
            r#"{"model":"test-model","messages":[{"role":"user","content":"hi"}]}"#,
        ))
        .await
        .expect("completion response");
    assert_eq!(completion.status(), StatusCode::OK);
    let text = body_text(completion).await;
    assert!(text.contains(r#""content":"hello""#));
    assert!(text.contains(r#""finish_reason":"stop""#));
    assert!(text.contains(r#""cached_tokens":0"#));
}

#[tokio::test]
async fn streaming_has_stable_chunks_usage_and_done() {
    let (router, _) = app(ApiConfig {
        model_id: "test-model".into(),
        ..ApiConfig::default()
    });
    let response = router
        .oneshot(chat_request(
            r#"{"model":"test-model","messages":[{"role":"user","content":"hi"}],"stream":true,"stream_options":{"include_usage":true}}"#,
        ))
        .await
        .expect("stream response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream")
    );
    let text = body_text(response).await;
    assert!(text.contains(r#""role":"assistant""#));
    assert!(text.contains(r#""content":"hello""#));
    assert!(text.contains(r#""finish_reason":"stop""#));
    assert!(text.contains(r#""prompt_tokens""#));
    assert!(text.ends_with("data: [DONE]\n\n"));
}

#[tokio::test]
async fn wrong_model_and_malformed_json_use_openai_errors() {
    let (router, _) = app(ApiConfig {
        model_id: "test-model".into(),
        ..ApiConfig::default()
    });
    let wrong_model = router
        .clone()
        .oneshot(chat_request(
            r#"{"model":"wrong","messages":[{"role":"user","content":"hi"}]}"#,
        ))
        .await
        .expect("wrong model response");
    assert_eq!(wrong_model.status(), StatusCode::NOT_FOUND);
    assert!(body_text(wrong_model).await.contains("model_not_found"));

    let malformed = router
        .oneshot(chat_request("{"))
        .await
        .expect("malformed response");
    assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);
    assert!(body_text(malformed).await.contains("invalid_json"));
}

#[tokio::test]
async fn mock_tool_and_failure_paths_use_openai_shapes() {
    let (router, _) = app(ApiConfig {
        model_id: "test-model".into(),
        ..ApiConfig::default()
    });
    let tool = router
        .clone()
        .oneshot(chat_request(
            r#"{"model":"test-model","messages":[{"role":"user","content":"[tool]"}]}"#,
        ))
        .await
        .expect("tool response");
    assert_eq!(tool.status(), StatusCode::OK);
    let tool_text = body_text(tool).await;
    assert!(tool_text.contains(r#""finish_reason":"tool_calls""#));
    assert!(tool_text.contains(r#""name":"mock_tool""#));

    let failure = router
        .oneshot(chat_request(
            r#"{"model":"test-model","messages":[{"role":"user","content":"[fail]"}]}"#,
        ))
        .await
        .expect("failure response");
    assert_eq!(failure.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert!(body_text(failure).await.contains("mock_error"));
}

#[tokio::test]
async fn invalid_sampling_values_use_field_specific_errors() {
    let (router, _) = app(ApiConfig {
        model_id: "test-model".into(),
        ..ApiConfig::default()
    });
    let response = router
        .oneshot(chat_request(
            r#"{"model":"test-model","messages":[{"role":"user","content":"hi"}],"temperature":3}"#,
        ))
        .await
        .expect("validation response");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let text = body_text(response).await;
    assert!(text.contains("invalid_value"));
    assert!(text.contains("temperature"));
}

#[tokio::test]
async fn body_limit_uses_openai_error_envelope() {
    let (router, _) = app(ApiConfig::default());
    let oversized = format!(
        r#"{{"model":"gemma-4-26b-a4b-it","messages":[{{"role":"user","content":"{}"}}]}}"#,
        "x".repeat(1024 * 1024)
    );
    let response = router
        .oneshot(chat_request(&oversized))
        .await
        .expect("large response");
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert!(body_text(response).await.contains("request_too_large"));
}

#[tokio::test]
async fn dropping_stream_cancels_slow_generation() {
    let (router, backend) = app(ApiConfig {
        model_id: "test-model".into(),
        mock_slow_delay: Duration::from_secs(30),
        ..ApiConfig::default()
    });
    let response = router
        .oneshot(chat_request(
            r#"{"model":"test-model","messages":[{"role":"user","content":"[slow]"}],"stream":true}"#,
        ))
        .await
        .expect("stream response");
    drop(response);
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if backend.stats().cancelled == 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("backend cancellation");
}

#[tokio::test]
async fn aborting_non_streaming_request_cancels_generation() {
    let (router, backend) = app(ApiConfig {
        model_id: "test-model".into(),
        mock_slow_delay: Duration::from_secs(30),
        ..ApiConfig::default()
    });
    let request =
        chat_request(r#"{"model":"test-model","messages":[{"role":"user","content":"[slow]"}]}"#);
    let task = tokio::spawn(async move { router.oneshot(request).await });
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if backend.stats().started == 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("backend start");
    task.abort();
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if backend.stats().cancelled == 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("backend cancellation");
}

#[tokio::test]
async fn bounded_channel_backpressures_mock_backend() {
    let backend = MockBackend::default();
    let (sender, mut receiver) = mpsc::channel::<GenerationEvent>(EVENT_CHANNEL_CAPACITY);
    let cancellation = CancellationToken::new();
    let task_cancellation = cancellation.clone();
    let task_backend = backend.clone();
    let task = tokio::spawn(async move {
        task_backend
            .generate(
                GenerationRequest {
                    request_id: "test".into(),
                    model: "test-model".into(),
                    prompt: "[many]".into(),
                    stream: true,
                    include_usage: false,
                },
                sender,
                task_cancellation,
                Duration::ZERO,
            )
            .await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(backend.stats().sent, EVENT_CHANNEL_CAPACITY);
    assert!(!task.is_finished());
    cancellation.cancel();
    while receiver.recv().await.is_some() {}
    task.await.expect("backend task");
    assert_eq!(backend.stats().cancelled, 1);
}
