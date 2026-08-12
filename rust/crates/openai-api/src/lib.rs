use std::{
    collections::VecDeque,
    convert::Infallible,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State, rejection::JsonRejection},
    http::{HeaderName, StatusCode},
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{get, post},
};
use futures_util::{Stream, stream};
use openai_protocol::{
    chat::ChatCompletionRequest, common::GenerationRequest as OpenAIGenerationRequest,
};
use serde::Serialize;
use tokio::sync::{Semaphore, mpsc};
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::{CancellationToken, DropGuard};
use tower_http::{
    request_id::{MakeRequestUuid, SetRequestIdLayer},
    trace::TraceLayer,
};
use tracing::{info, warn};
use turbofieldfare_protocol::{GenerationEvent, GenerationRequest, Usage};
use uuid::Uuid;

const REQUEST_BODY_LIMIT: usize = 1024 * 1024;
const EVENT_CHANNEL_CAPACITY: usize = 8;

#[derive(Debug, Clone)]
pub struct ApiConfig {
    pub model_id: String,
    pub queue_limit: usize,
    pub heartbeat_interval: Duration,
    pub mock_slow_delay: Duration,
}

impl Default for ApiConfig {
    fn default() -> Self {
        Self {
            model_id: "gemma-4-26b-a4b-it".into(),
            queue_limit: 8,
            heartbeat_interval: Duration::from_secs(5),
            mock_slow_delay: Duration::from_millis(250),
        }
    }
}

#[derive(Clone)]
struct AppState {
    config: ApiConfig,
    backend: Arc<dyn InferenceBackend>,
    admission: Arc<Semaphore>,
    generation: Arc<Semaphore>,
}

#[derive(Debug, Clone, Default)]
pub struct MockBackend {
    stats: Arc<MockStats>,
}

#[derive(Debug, Default)]
struct MockStats {
    started: AtomicUsize,
    completed: AtomicUsize,
    cancelled: AtomicUsize,
    sent: AtomicUsize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MockStatsSnapshot {
    pub started: usize,
    pub completed: usize,
    pub cancelled: usize,
    pub sent: usize,
}

impl MockBackend {
    #[must_use]
    pub fn stats(&self) -> MockStatsSnapshot {
        MockStatsSnapshot {
            started: self.stats.started.load(Ordering::Relaxed),
            completed: self.stats.completed.load(Ordering::Relaxed),
            cancelled: self.stats.cancelled.load(Ordering::Relaxed),
            sent: self.stats.sent.load(Ordering::Relaxed),
        }
    }

    async fn generate_mock(
        &self,
        request: GenerationRequest,
        sender: mpsc::Sender<GenerationEvent>,
        cancellation: CancellationToken,
        slow_delay: Duration,
    ) {
        self.stats.started.fetch_add(1, Ordering::Relaxed);
        let result = self
            .generate_inner(&request, &sender, &cancellation, slow_delay)
            .await;
        match result {
            Ok(()) => {
                self.stats.completed.fetch_add(1, Ordering::Relaxed);
                info!(request_id = request.request_id, phase = "completed");
            }
            Err(BackendStop::Cancelled) => {
                self.stats.cancelled.fetch_add(1, Ordering::Relaxed);
                info!(request_id = request.request_id, phase = "cancelled");
            }
            Err(BackendStop::ReceiverDropped) => {
                self.stats.cancelled.fetch_add(1, Ordering::Relaxed);
                info!(request_id = request.request_id, phase = "disconnected");
            }
        }
    }

    async fn generate_inner(
        &self,
        request: &GenerationRequest,
        sender: &mpsc::Sender<GenerationEvent>,
        cancellation: &CancellationToken,
        slow_delay: Duration,
    ) -> Result<(), BackendStop> {
        let prompt_tokens = u32::try_from(request.prompt.split_whitespace().count())
            .unwrap_or(u32::MAX)
            .max(1);
        self.send(
            sender,
            cancellation,
            GenerationEvent::Prepared { prompt_tokens },
        )
        .await?;
        info!(
            request_id = request.request_id,
            phase = "prepared",
            prompt_tokens
        );

        if request.prompt.contains("[slow]") {
            tokio::select! {
                () = cancellation.cancelled() => return Err(BackendStop::Cancelled),
                () = tokio::time::sleep(slow_delay) => {}
            }
        }
        if request.prompt.contains("[fail]") {
            self.send(
                sender,
                cancellation,
                GenerationEvent::Failed {
                    message: "mock generation failed".into(),
                    code: "mock_error".into(),
                },
            )
            .await?;
            return Ok(());
        }

        let wants_tool = request.prompt.contains("[tool]");
        let chunk_count = if request.prompt.contains("[many]") {
            100
        } else {
            1
        };
        for _ in 0..chunk_count {
            self.send(
                sender,
                cancellation,
                GenerationEvent::Content {
                    text: "hello".into(),
                },
            )
            .await?;
        }

        if wants_tool {
            self.send(
                sender,
                cancellation,
                GenerationEvent::ToolCall {
                    id: "call_000000000000000000000001".into(),
                    name: "mock_tool".into(),
                    arguments: r#"{"value":"mock"}"#.into(),
                },
            )
            .await?;
        }
        let completion_tokens = if wants_tool {
            8
        } else {
            u32::try_from(chunk_count).unwrap_or(u32::MAX)
        };
        self.send(
            sender,
            cancellation,
            GenerationEvent::Completed {
                finish_reason: if wants_tool { "tool_calls" } else { "stop" }.into(),
                usage: Usage {
                    prompt_tokens,
                    completion_tokens,
                    cached_tokens: 0,
                },
            },
        )
        .await
    }

    async fn send(
        &self,
        sender: &mpsc::Sender<GenerationEvent>,
        cancellation: &CancellationToken,
        event: GenerationEvent,
    ) -> Result<(), BackendStop> {
        tokio::select! {
            () = cancellation.cancelled() => Err(BackendStop::Cancelled),
            result = sender.send(event) => {
                result.map_err(|_| BackendStop::ReceiverDropped)?;
                self.stats.sent.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
        }
    }
}

#[async_trait]
pub trait InferenceBackend: Send + Sync {
    async fn generate(
        &self,
        request: GenerationRequest,
        sender: mpsc::Sender<GenerationEvent>,
        cancellation: CancellationToken,
        slow_delay: Duration,
    );
}

#[async_trait]
impl InferenceBackend for MockBackend {
    async fn generate(
        &self,
        request: GenerationRequest,
        sender: mpsc::Sender<GenerationEvent>,
        cancellation: CancellationToken,
        slow_delay: Duration,
    ) {
        self.generate_mock(request, sender, cancellation, slow_delay)
            .await;
    }
}

#[derive(Debug, Clone, Copy)]
enum BackendStop {
    Cancelled,
    ReceiverDropped,
}

pub fn app(config: ApiConfig) -> (Router, MockBackend) {
    let backend = MockBackend::default();
    let admission_capacity = config.queue_limit.saturating_add(1);
    let state = AppState {
        config,
        backend: Arc::new(backend.clone()),
        admission: Arc::new(Semaphore::new(admission_capacity)),
        generation: Arc::new(Semaphore::new(1)),
    };
    let router = Router::new()
        .route("/health", get(health))
        .route("/v1/models", get(models))
        .route("/v1/chat/completions", post(chat_completions))
        .fallback(route_not_found)
        .method_not_allowed_fallback(method_not_allowed)
        .layer(DefaultBodyLimit::max(REQUEST_BODY_LIMIT))
        .layer(SetRequestIdLayer::new(
            HeaderName::from_static("x-request-id"),
            MakeRequestUuid,
        ))
        .layer(TraceLayer::new_for_http())
        .with_state(state);
    (router, backend)
}

/// Runs the mock `OpenAI` server until Ctrl-C is received.
///
/// # Errors
///
/// Returns an error when the address is not loopback, the listener cannot be
/// bound, or the HTTP server terminates with an I/O error.
pub async fn serve(address: SocketAddr, config: ApiConfig) -> std::io::Result<()> {
    if !address.ip().is_loopback() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "server address must be loopback",
        ));
    }
    let listener = tokio::net::TcpListener::bind(address).await?;
    info!(address = %listener.local_addr()?, "Rust OpenAI mock server ready");
    axum::serve(listener, app(config).0)
        .with_graceful_shutdown(shutdown_signal())
        .await
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

async fn models(State(state): State<AppState>) -> Json<ModelList> {
    Json(ModelList {
        object: "list",
        data: vec![Model {
            id: state.config.model_id,
            object: "model",
            created: 0,
            owned_by: "turbofieldfare",
        }],
    })
}

async fn chat_completions(
    State(state): State<AppState>,
    request: Result<Json<ChatCompletionRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(request) = request.map_err(|error| ApiError::from_json_rejection(&error))?;
    validate_request(&request, &state.config.model_id)?;
    let request_id = format!("chatcmpl-{}", Uuid::new_v4().simple());
    let created = unix_timestamp();
    let include_usage = request
        .stream_options
        .as_ref()
        .and_then(|options| options.include_usage)
        .unwrap_or(false);
    let protocol_request = GenerationRequest {
        request_id: request_id.clone(),
        model: request.model.clone(),
        prompt: request.extract_text_for_routing(),
        stream: request.stream,
        include_usage,
    };
    let admission = state
        .admission
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::queue_full())?;
    info!(request_id, phase = "accepted", streaming = request.stream);

    if request.stream {
        Ok(
            streaming_response(state, protocol_request, request_id, created, admission)
                .into_response(),
        )
    } else {
        non_streaming_response(state, protocol_request, request_id, created, admission).await
    }
}

async fn non_streaming_response(
    state: AppState,
    request: GenerationRequest,
    response_id: String,
    created: u64,
    _admission: tokio::sync::OwnedSemaphorePermit,
) -> Result<Response, ApiError> {
    let _generation = state
        .generation
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| ApiError::internal())?;
    info!(request_id = response_id, phase = "generating");
    let (sender, mut receiver) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
    let cancellation = CancellationToken::new();
    let _cancel_on_drop = cancellation.clone().drop_guard();
    let backend = state.backend.clone();
    let delay = state.config.mock_slow_delay;
    let task = tokio::spawn(async move {
        backend.generate(request, sender, cancellation, delay).await;
    });
    let mut content = String::new();
    let mut tool_calls = Vec::new();
    let mut completed = None;
    while let Some(event) = receiver.recv().await {
        match event {
            GenerationEvent::Prepared { .. } => {}
            GenerationEvent::Content { text } => content.push_str(&text),
            GenerationEvent::ToolCall {
                id,
                name,
                arguments,
            } => {
                tool_calls.push(ToolCallResponse::new(id, name, arguments));
            }
            GenerationEvent::Completed {
                finish_reason,
                usage,
            } => {
                completed = Some((finish_reason, usage));
                break;
            }
            GenerationEvent::Failed { message, code } => {
                task.await.map_err(|_| ApiError::internal())?;
                return Err(ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    message,
                    None,
                    code,
                ));
            }
        }
    }
    task.await.map_err(|_| ApiError::internal())?;
    let (finish_reason, usage) = completed.ok_or_else(ApiError::internal)?;
    let response = ChatResponse {
        id: response_id,
        object: "chat.completion",
        created,
        model: state.config.model_id,
        choices: vec![ChatChoice {
            index: 0,
            message: AssistantMessage {
                role: "assistant",
                content: if content.is_empty() && !tool_calls.is_empty() {
                    None
                } else {
                    Some(content)
                },
                tool_calls: if tool_calls.is_empty() {
                    None
                } else {
                    Some(tool_calls)
                },
            },
            finish_reason,
        }],
        usage: UsageResponse::from(usage),
    };
    Ok(Json(response).into_response())
}

fn streaming_response(
    state: AppState,
    request: GenerationRequest,
    response_id: String,
    created: u64,
    admission: tokio::sync::OwnedSemaphorePermit,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let (sender, receiver) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
    let include_usage = request.include_usage;
    let cancellation = CancellationToken::new();
    let cancellation_for_task = cancellation.clone();
    let backend = state.backend.clone();
    let generation = state.generation.clone();
    let delay = state.config.mock_slow_delay;
    let request_id = response_id.clone();
    tokio::spawn(async move {
        let Ok(_generation) = generation.acquire_owned().await else {
            return;
        };
        info!(request_id, phase = "generating");
        backend
            .generate(request, sender, cancellation_for_task, delay)
            .await;
        drop(admission);
    });

    let mut pending = VecDeque::new();
    pending.push_back(sse_json(&StreamChunk::role(
        &response_id,
        created,
        &state.config.model_id,
    )));
    let stream_state = SseState {
        receiver: ReceiverStream::new(receiver),
        _cancel_on_drop: cancellation.drop_guard(),
        pending,
        id: response_id,
        created,
        model: state.config.model_id,
        include_usage,
        tool_index: 0,
    };
    let stream = stream::unfold(stream_state, |mut state| async move {
        loop {
            if let Some(event) = state.pending.pop_front() {
                return Some((Ok(event), state));
            }
            let event = tokio_stream::StreamExt::next(&mut state.receiver).await?;
            match event {
                GenerationEvent::Prepared { .. } => {}
                GenerationEvent::Content { text } => {
                    let chunk = StreamChunk::content(&state.id, state.created, &state.model, text);
                    return Some((Ok(sse_json(&chunk)), state));
                }
                GenerationEvent::ToolCall {
                    id,
                    name,
                    arguments,
                } => {
                    let chunk = StreamChunk::tool(
                        &state.id,
                        state.created,
                        &state.model,
                        state.tool_index,
                        id,
                        name,
                        arguments,
                    );
                    state.tool_index += 1;
                    return Some((Ok(sse_json(&chunk)), state));
                }
                GenerationEvent::Completed {
                    finish_reason,
                    usage,
                } => {
                    state.pending.push_back(sse_json(&StreamChunk::finish(
                        &state.id,
                        state.created,
                        &state.model,
                        finish_reason,
                    )));
                    if state.include_usage {
                        state.pending.push_back(sse_json(&UsageChunk::new(
                            &state.id,
                            state.created,
                            &state.model,
                            usage,
                        )));
                    }
                    state.pending.push_back(Event::default().data("[DONE]"));
                }
                GenerationEvent::Failed { message, code } => {
                    state
                        .pending
                        .push_back(sse_json(&ErrorEnvelope::new(message, None, code)));
                    state.pending.push_back(Event::default().data("[DONE]"));
                }
            }
        }
    });
    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(state.config.heartbeat_interval)
            .text("ping"),
    )
}

struct SseState {
    receiver: ReceiverStream<GenerationEvent>,
    _cancel_on_drop: DropGuard,
    pending: VecDeque<Event>,
    id: String,
    created: u64,
    model: String,
    include_usage: bool,
    tool_index: u32,
}

fn validate_request(request: &ChatCompletionRequest, model_id: &str) -> Result<(), ApiError> {
    if request.model != model_id {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "requested model is not available",
            Some("model"),
            "model_not_found",
        ));
    }
    if request.messages.is_empty() {
        return Err(ApiError::invalid(
            "messages must not be empty",
            Some("messages"),
            "invalid_value",
        ));
    }
    if request.n.is_some_and(|n| n != 1) {
        return Err(ApiError::invalid(
            "only n=1 is supported",
            Some("n"),
            "unsupported_value",
        ));
    }
    if request.logprobs {
        return Err(ApiError::invalid(
            "logprobs are not supported",
            Some("logprobs"),
            "unsupported_value",
        ));
    }
    if request.presence_penalty.is_some_and(|value| value != 0.0) {
        return Err(ApiError::invalid(
            "presence_penalty must be zero",
            Some("presence_penalty"),
            "unsupported_value",
        ));
    }
    if request.frequency_penalty.is_some_and(|value| value != 0.0) {
        return Err(ApiError::invalid(
            "frequency_penalty must be zero",
            Some("frequency_penalty"),
            "unsupported_value",
        ));
    }
    if request.parallel_tool_calls == Some(false) {
        return Err(ApiError::invalid(
            "parallel_tool_calls=false is not supported",
            Some("parallel_tool_calls"),
            "unsupported_value",
        ));
    }
    validate_sampling(request)
}

fn validate_sampling(request: &ChatCompletionRequest) -> Result<(), ApiError> {
    if request
        .temperature
        .is_some_and(|value| !(0.0..=2.0).contains(&value))
    {
        return Err(ApiError::invalid(
            "temperature must be between 0 and 2",
            Some("temperature"),
            "invalid_value",
        ));
    }
    if request
        .top_p
        .is_some_and(|value| !(value > 0.0 && value <= 1.0))
    {
        return Err(ApiError::invalid(
            "top_p must be greater than 0 and at most 1",
            Some("top_p"),
            "invalid_value",
        ));
    }
    if request
        .top_k
        .is_some_and(|value| !(1..=256).contains(&value))
    {
        return Err(ApiError::invalid(
            "top_k must be between 1 and 256",
            Some("top_k"),
            "invalid_value",
        ));
    }
    if request.repetition_penalty.is_some_and(|value| value <= 0.0) {
        return Err(ApiError::invalid(
            "repetition_penalty must be positive",
            Some("repetition_penalty"),
            "invalid_value",
        ));
    }
    if requested_max_tokens(request).is_some_and(|value| value == 0) {
        let parameter = if request.max_completion_tokens.is_some() {
            "max_completion_tokens"
        } else {
            "max_tokens"
        };
        return Err(ApiError::invalid(
            "maximum completion tokens must be positive",
            Some(parameter),
            "invalid_value",
        ));
    }
    Ok(())
}

#[allow(deprecated)]
fn requested_max_tokens(request: &ChatCompletionRequest) -> Option<u32> {
    request.max_completion_tokens.or(request.max_tokens)
}

async fn route_not_found() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "route not found", None, "not_found")
}

async fn method_not_allowed() -> ApiError {
    ApiError::new(
        StatusCode::METHOD_NOT_ALLOWED,
        "method not allowed",
        None,
        "method_not_allowed",
    )
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    envelope: ErrorEnvelope,
}

impl ApiError {
    fn new(
        status: StatusCode,
        message: impl Into<String>,
        param: Option<&str>,
        code: impl Into<String>,
    ) -> Self {
        Self {
            status,
            envelope: ErrorEnvelope::new(message, param, code),
        }
    }

    fn invalid(message: &'static str, param: Option<&str>, code: &'static str) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message, param, code)
    }

    fn queue_full() -> Self {
        Self::new(
            StatusCode::TOO_MANY_REQUESTS,
            "generation queue is full",
            None,
            "queue_full",
        )
    }

    fn internal() -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal server error",
            None,
            "internal_error",
        )
    }

    fn from_json_rejection(rejection: &JsonRejection) -> Self {
        let status = rejection.status();
        if status == StatusCode::PAYLOAD_TOO_LARGE {
            Self::new(
                status,
                "request body is too large",
                None,
                "request_too_large",
            )
        } else if status == StatusCode::UNSUPPORTED_MEDIA_TYPE {
            Self::new(
                status,
                "content-type must be application/json",
                None,
                "unsupported_media_type",
            )
        } else {
            Self::new(
                StatusCode::BAD_REQUEST,
                "malformed JSON request",
                None,
                "invalid_json",
            )
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        warn!(status = %self.status, code = self.envelope.error.code, "request failed");
        (self.status, Json(self.envelope)).into_response()
    }
}

#[derive(Debug, Serialize)]
struct ErrorEnvelope {
    error: ErrorDetail,
}

impl ErrorEnvelope {
    fn new(message: impl Into<String>, param: Option<&str>, code: impl Into<String>) -> Self {
        Self {
            error: ErrorDetail {
                message: message.into(),
                error_type: "invalid_request_error",
                param: param.map(str::to_owned),
                code: code.into(),
            },
        }
    }
}

#[derive(Debug, Serialize)]
struct ErrorDetail {
    message: String,
    #[serde(rename = "type")]
    error_type: &'static str,
    param: Option<String>,
    code: String,
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
}

#[derive(Serialize)]
struct ModelList {
    object: &'static str,
    data: Vec<Model>,
}

#[derive(Serialize)]
struct Model {
    id: String,
    object: &'static str,
    created: u64,
    owned_by: &'static str,
}

#[derive(Serialize)]
struct ChatResponse {
    id: String,
    object: &'static str,
    created: u64,
    model: String,
    choices: Vec<ChatChoice>,
    usage: UsageResponse,
}

#[derive(Serialize)]
struct ChatChoice {
    index: u32,
    message: AssistantMessage,
    finish_reason: String,
}

#[derive(Serialize)]
struct AssistantMessage {
    role: &'static str,
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<ToolCallResponse>>,
}

#[derive(Serialize)]
struct ToolCallResponse {
    id: String,
    #[serde(rename = "type")]
    tool_type: &'static str,
    function: FunctionCallResponse,
}

impl ToolCallResponse {
    fn new(id: String, name: String, arguments: String) -> Self {
        Self {
            id,
            tool_type: "function",
            function: FunctionCallResponse { name, arguments },
        }
    }
}

#[derive(Serialize)]
struct FunctionCallResponse {
    name: String,
    arguments: String,
}

#[derive(Serialize)]
struct UsageResponse {
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
    prompt_tokens_details: PromptTokenDetails,
}

impl From<Usage> for UsageResponse {
    fn from(usage: Usage) -> Self {
        Self {
            prompt_tokens: usage.prompt_tokens,
            completion_tokens: usage.completion_tokens,
            total_tokens: usage.total_tokens(),
            prompt_tokens_details: PromptTokenDetails {
                cached_tokens: usage.cached_tokens,
            },
        }
    }
}

#[derive(Serialize)]
struct PromptTokenDetails {
    cached_tokens: u32,
}

#[derive(Serialize)]
struct StreamChunk {
    id: String,
    object: &'static str,
    created: u64,
    model: String,
    choices: Vec<StreamChoice>,
}

impl StreamChunk {
    fn role(id: &str, created: u64, model: &str) -> Self {
        Self::new(id, created, model, Delta::role(), None)
    }

    fn content(id: &str, created: u64, model: &str, text: String) -> Self {
        Self::new(id, created, model, Delta::content(text), None)
    }

    fn tool(
        id: &str,
        created: u64,
        model: &str,
        index: u32,
        call_id: String,
        name: String,
        arguments: String,
    ) -> Self {
        Self::new(
            id,
            created,
            model,
            Delta::tool(index, call_id, name, arguments),
            None,
        )
    }

    fn finish(id: &str, created: u64, model: &str, finish_reason: String) -> Self {
        Self::new(id, created, model, Delta::default(), Some(finish_reason))
    }

    fn new(
        id: &str,
        created: u64,
        model: &str,
        delta: Delta,
        finish_reason: Option<String>,
    ) -> Self {
        Self {
            id: id.into(),
            object: "chat.completion.chunk",
            created,
            model: model.into(),
            choices: vec![StreamChoice {
                index: 0,
                delta,
                finish_reason,
            }],
        }
    }
}

#[derive(Serialize)]
struct StreamChoice {
    index: u32,
    delta: Delta,
    finish_reason: Option<String>,
}

#[derive(Default, Serialize)]
struct Delta {
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<ToolCallDelta>>,
}

impl Delta {
    fn role() -> Self {
        Self {
            role: Some("assistant"),
            ..Self::default()
        }
    }

    fn content(text: String) -> Self {
        Self {
            content: Some(text),
            ..Self::default()
        }
    }

    fn tool(index: u32, id: String, name: String, arguments: String) -> Self {
        Self {
            tool_calls: Some(vec![ToolCallDelta {
                index,
                id,
                tool_type: "function",
                function: FunctionCallResponse { name, arguments },
            }]),
            ..Self::default()
        }
    }
}

#[derive(Serialize)]
struct ToolCallDelta {
    index: u32,
    id: String,
    #[serde(rename = "type")]
    tool_type: &'static str,
    function: FunctionCallResponse,
}

#[derive(Serialize)]
struct UsageChunk {
    id: String,
    object: &'static str,
    created: u64,
    model: String,
    choices: [(); 0],
    usage: UsageResponse,
}

impl UsageChunk {
    fn new(id: &str, created: u64, model: &str, usage: Usage) -> Self {
        Self {
            id: id.into(),
            object: "chat.completion.chunk",
            created,
            model: model.into(),
            choices: [],
            usage: usage.into(),
        }
    }
}

fn sse_json(value: &impl Serialize) -> Event {
    Event::default().json_data(value).unwrap_or_else(|_| {
        Event::default().data(
            r#"{"error":{"message":"stream response could not be encoded","type":"invalid_request_error","param":null,"code":"internal_error"}}"#,
        )
    })
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests;
