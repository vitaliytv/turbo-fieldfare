# Rust migration and learning plan

## Outcome

The final product is terminal-only and implemented in Rust plus Metal Shading
Language:

```text
OpenAI clients
    |
    | HTTP/1.1, JSON, SSE
    v
turbofieldfare-api (Rust)
    |
    | versioned local IPC
    v
turbofieldfare-worker (Rust)
    |
    v
Metal 4 / TensorOps / .gturbo
```

The migration is deliberately hybrid. The OpenAI-compatible server is
separated first and connected to the existing Swift/Metal inference runtime.
The inference worker is then ported behind the same IPC contract. This keeps a
working product and a correctness oracle throughout the Rust and inference
learning process.

This plan has two distinct tracks:

1. **Behavioral migration:** reproduce the shipping behavior in Rust while
   reusing the `.gturbo` format and existing Metal kernels.
2. **Metal modernization:** after parity, evaluate newer Apple APIs and promote
   only changes that pass correctness, quality, memory, and wall-time gates.

Do not combine a language port with an optimization in the same milestone.

## Guiding constraints

- Keep the public server on `127.0.0.1`; it has no remote authentication or
  TLS.
- Load the model in exactly one worker process.
- Keep the authoritative one-active-generation queue in the worker. The API
  may impose admission limits, but it must not be the only serialization
  boundary.
- Never transfer logits, KV buffers, weights, or other model-scale data over
  IPC. Transfer normalized requests and text/tool/usage events only.
- Preserve bounded memory, expert streaming, integrity checks, cancellation,
  prompt-cache behavior, and Files/Batch semantics.
- Preserve a Swift reference implementation until the Rust worker clears all
  parity gates.
- Keep existing `.metal` kernels during the host-language migration. Kernel
  changes are separate experiments.
- Do not delete UI, Swift server, CLI, repacker, or reference tests merely to
  simplify an intermediate milestone.

## Apple platform baseline

Use the project's [implementation references](IMPLEMENTATION_REFERENCES.md) as
the maintained source index. The migration currently follows these Apple
contracts and practices:

- [Metal Performance Primitives programming guide](https://developer.apple.com/download/files/Metal-Performance-Primitives-Programming-Guide.pdf)
  for TensorOps tiling, cooperative tensors, data types, execution scopes, and
  alignment.
- [Optimize custom machine learning operations with Metal tensors](https://developer.apple.com/videos/play/wwdc2026/330/)
  for quantized tensors, TensorOps, cooperative reuse, and custom attention.
- [Discover Metal 4](https://developer.apple.com/videos/play/wwdc2025/205/)
  for modular adoption, command allocation, residency, barriers, and shader
  compilation.
- [Metal resource loading](https://developer.apple.com/documentation/metal/resource-loading)
  for evaluating Metal I/O queues against the existing bounded `pread` design.
- [Metal developer tools](https://developer.apple.com/metal/tools/)
  for API/Shader Validation, Metal Debugger, and Metal System Trace.

Recheck these sources and the Metal feature-set tables when raising the minimum
macOS/Xcode version or adopting a new GPU-family path. A newer API is a
candidate, not an automatic replacement for a measured production path.

## Target Cargo workspace

The workspace grows incrementally; all crates do not need to exist on day one.

```text
rust/
  Cargo.toml
  crates/
    protocol/          versioned IPC DTOs and shared fixtures
    openai-api/        Axum routes, SSE, Files, and Batch
    model-format/      .gturbo manifest, layout, and integrity
    tokenizer/         tokenizer, chat template, and tool parsing
    metal-runtime/     safe project API over objc2-metal
    kernels/           Rust host wrappers for existing MSL kernels
    inference/         model, KV cache, prefill, decode, and sampling
    worker/            inference IPC service
    repack/            bounded streaming installer
    cli/               install, run, serve, inspect, compare, benchmark
```

The intended dependency direction is:

```text
cli -> openai-api -> protocol <- worker -> inference
                                      inference -> tokenizer
                                      inference -> model-format
                                      inference -> kernels -> metal-runtime
repack -> model-format
```

`protocol` must not depend on Axum, OpenAI SDK types, Swift types, or Metal.

## Reusable Rust ecosystem

Pin exact versions in `Cargo.lock` and review upgrades deliberately.

| Concern | Initial crate choice |
| --- | --- |
| Async runtime | `tokio` |
| HTTP routing | `axum` |
| HTTP limits, request IDs, tracing | `tower-http` |
| OpenAI wire types | `openai-protocol` behind a local adapter |
| Serialization | `serde`, `serde_json` |
| IPC framing | `tokio-util` |
| Cancellation and streams | `tokio-util`, `tokio-stream`, `futures-util` |
| CLI | `clap` |
| Errors and diagnostics | `thiserror`, `tracing`, `tracing-subscriber` |
| Metal bindings | `objc2`, `objc2-foundation`, `objc2-metal` |
| FP16/BF16 and POD data | `half`, `bytemuck` |
| Memory maps and POSIX I/O | `memmap2`, `rustix` |
| Tokenization and templates | `tokenizers`, `minijinja` |
| Hub/range transport | `hf-hub`, `reqwest` |
| Model sources | `safetensors` where source parsing benefits |
| Integrity | `sha2` |
| Tests and microbenchmarks | `proptest`, `criterion` |

`openai-protocol` saves wire-type work, but does not replace
TurboFieldfare-specific validation. Keep local rules for model IDs, `n=1`,
sampling ranges, unsupported fields, tool choices, Gemma schema adaptation,
historical tool calls, and actual context limits.

The older `metal` crate is deprecated. New Rust Metal work uses `objc2-metal`
and confines Objective-C lifetime handling and `unsafe` code to
`metal-runtime`.

## Phase 0: freeze the reference

### Learning goals

- Distinguish exact output identity, numerical tolerance, model-quality parity,
  and product-level HTTP compatibility.
- Learn the current `.gturbo`, prefill, decode, MoE streaming, and measurement
  contracts before translating them.

### Deliverables

- A named Swift reference commit and model-manifest hash.
- Black-box HTTP fixtures for routes, errors, SSE, tools, Files, and Batch.
- Tokenizer and rendered-prompt fixtures.
- Kernel input/output fixtures and independent FP32 references.
- Fixed-seed generation fixtures.
- A recorded performance baseline using the existing community benchmark
  protocol.

Record commit, hardware, RAM, macOS, Swift/Rust/Xcode versions, exact commands,
exit codes, timing footers, energy mode, and protocol deviations.

### Exit gate

The same external contract suite can run against an arbitrary base URL, and
the reference benchmark can be repeated without editing source.

## Phase 1: Rust OpenAI server with a mock backend

Start with only:

```text
protocol
openai-api
cli
```

Implement:

- `GET /health`
- `GET /v1/models`
- `POST /v1/chat/completions`
- non-streaming responses
- SSE chunks, heartbeat, usage chunk, and `[DONE]`
- OpenAI error envelopes
- 1 MiB normal request-body limit
- request cancellation and bounded backpressure
- structured request and phase logging

The mock backend emits deterministic `prepared`, `content`, `tool_call`,
`completed`, and `failed` events. It must support slow-client and cancellation
tests without a model.

### Learning goals

- Rust ownership, enums, traits, and error handling.
- Tokio tasks, channels, cancellation, and stream lifetimes.
- Axum extractors/responses and SSE backpressure.
- Serde compatibility and protocol-oriented testing.

### Exit gate

- `cargo fmt --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`
- HTTP contract fixtures pass for implemented routes.
- Disconnecting a streaming client cancels the mock generation.
- Slow clients cannot cause unbounded buffering.

## Phase 2: versioned IPC v1

Use a Unix domain socket with a fixed-size length prefix and JSON payload. JSON
keeps the first protocol easy to inspect from both Swift and Rust; framing
removes newline ambiguity.

Every frame includes `protocol_version`, `type`, and `request_id` where
applicable. Initial messages are:

```text
hello
ready / incompatible
generate
cancel
prepared
content
tool_call
completed
failed
ping / pong
shutdown
```

`ready` advertises worker capabilities such as model ID, maximum context,
tool support, prompt-cache mode, and maximum concurrency. The protocol includes
explicit queue-full, worker-busy, invalid-request, model-error, cancelled, and
internal-error categories.

### Ownership boundary

Rust API owns:

- HTTP and OpenAI wire compatibility
- generic OpenAI field validation
- SSE and response envelopes
- request IDs, body limits, Files, and Batch

Inference worker owns:

- tokenizer and Gemma chat template
- Gemma-specific tool-schema validation
- actual token-context validation
- generation queue and prompt/KV cache
- sampling, inference, Metal, and usage accounting

### Exit gate

- Rust protocol round-trip and malformed-frame tests pass.
- Swift and Rust decode the same golden frames.
- Unknown protocol versions fail before generation.
- Frame and queue sizes are bounded.
- Cancellation is idempotent and correlated by request ID.

## Phase 3: Swift inference worker

Create a terminal-only `TurboFieldfareWorker` executable over the existing
`ServerModelSession`. It opens no TCP port and contains no OpenAI response
formatting.

Startup sequence:

1. Bind or connect to the configured Unix socket.
2. Load and verify one `.gturbo` model.
3. Publish `ready` only after the model and tokenizer are usable.
4. Serialize all real inference through the worker coordinator.
5. Stream events and honor cancellation.
6. Shut down cleanly on the owning supervisor's signal.

### Exit gate

- Rust API plus Swift worker passes the old Swift server's HTTP contract suite.
- Text, Unicode, tool calls, finish reasons, errors, and usage match.
- Interactive and Batch work share the same authoritative worker queue.
- A worker crash becomes a controlled HTTP `503`.
- An API restart can reconnect without loading a second model.
- TTFT regression is at most 5%.
- Decode throughput remains at least 98% of the in-process Swift baseline.
- IPC memory remains bounded under long responses and slow clients.

At this point, Rust is the primary OpenAI-compatible server. Keep the SwiftNIO
server as a reference until Files/Batch and failure behavior have full parity.

## Phase 4: move Files and Batch to Rust

Implement the remaining API surface in `openai-api`:

- multipart file upload with an explicit 200 MiB limit
- file list, status, content, and delete
- JSONL validation
- Batch create, list, status, cancel, expiry, and pagination
- output and error JSONL files
- current metadata and compatibility limits

Batch submits ordinary generation requests over IPC. It never bypasses the
worker's serialization boundary.

### Exit gate

- Full black-box HTTP contract suite passes against Rust.
- Batch status transitions and result/error JSONL match.
- Cancellation reaches queued and active work correctly.
- Server restart semantics are explicit and tested.
- SwiftNIO is no longer needed in the production launch path.

## Phase 5: Rust `.gturbo` and tokenizer layer

Port without Metal execution:

- manifest and verified-install receipt parsing
- architecture and quantization validation
- resident index and packed-expert layout
- file sizes, hashes, offsets, strides, and alignment
- tokenizer load, encode, decode, and streaming decode
- pinned Jinja chat template
- structured assistant/tool-call parsing

Keep `.gturbo` unchanged. A simultaneous model-format redesign would remove the
ability to compare hosts independently.

### Exit gate

- Manifest and layout interpretation match Swift exactly.
- Token IDs and rendered prompts match golden fixtures.
- Tool-call structures match.
- Corrupt, truncated, misaligned, or incompatible models fail closed.
- No test or loader stages a model-scale tensor in the Rust heap.

## Phase 6: Rust Metal foundation

Create a small safe project API over `objc2-metal` for:

- device and capability discovery
- command queues, command buffers, and compute encoders
- buffers and no-copy ownership
- library/function/pipeline creation
- function constants
- dispatch geometry
- completion, error propagation, and synchronization
- labels and diagnostics

Confine Objective-C object-lifetime handling and all unavoidable `unsafe` to
this crate. Document every `unsafe` block's ownership, alignment, lifetime, and
synchronization invariants.

Learning exercises progress through vector addition, buffer round trips,
function constants, pipeline caching, and error/cancellation handling before
any model kernel.

Production builds ship an ahead-of-time compiled `.metallib`. Runtime source
compilation may remain as an explicit development fallback. Install and pin the
Xcode Metal Toolchain before this phase.

### Exit gate

- Metal API and Shader Validation are clean.
- CPU reference tests pass.
- Resource drops cannot race in-flight GPU work.
- Pipeline compilation and cache errors are observable.
- Release startup does not require runtime MSL compilation.

## Phase 7: port kernel host wrappers

Reuse the existing MSL and replace only Swift host orchestration. Recommended
order:

1. embedding lookup
2. RMSNorm
3. RoPE
4. INT8 affine GEMV
5. INT4 affine GEMV
6. logit softcap and sampling
7. attention
8. fused QKV paths
9. shared expert
10. routed MoE
11. fused layer tail and language-model head

Compare three independent paths where practical:

```text
CPU FP32 reference
Swift host + production MSL
Rust host + production MSL
```

Unchanged kernels and inputs should produce exact or established-tolerance
parity. Reordered TensorOps kernels require a direct numerical oracle plus
model-quality gates, not identity with one reduction order.

## Phase 8: model memory and expert streaming

Port:

- read-only mapping of common weights
- no-copy Metal buffer wrapping
- aligned expert-slot allocation
- positional `pread`
- lazy layer opening and verification
- 16-slot LFU cache with recency tie-break
- parallel bounded reads
- slot ownership until all GPU consumers finish
- cached-hit and missing-expert scheduling

First reproduce the current `pread` path. Evaluate `MTLIOCommandQueue` only as
a separate control/candidate experiment; its existence does not prove it is
better for small dynamically selected MoE experts.

### Exit gate

- Slot bytes and cache plans match Swift fixtures.
- No slot is reused before GPU completion.
- Cancellation cannot strand a slot in an ambiguous state.
- Cold and warm I/O behavior is measured separately.
- Physical footprint and file-cache effects are reported separately.

## Phase 9: Rust decode worker

Port:

- model ownership and runtime configuration
- FP16 sliding-window and full-attention KV caches
- 30-layer forward loop
- `cb1 -> CPU top-8 -> expert I/O -> cb2` scheduling
- shared-expert/read overlap
- tied head, sampling, stops, and detokenization
- prompt continuation and cancellation

The first correct end-to-end milestone may prefill token by token through the
decode path. It is intentionally slow but isolates decode correctness before
chunked prefill complexity.

Expose the Rust worker through the same IPC v1 contract:

```bash
turbofieldfare serve --worker swift
turbofieldfare serve --worker rust
```

### Exit gate

- First-token and fixed-prefix token parity pass where operation order matches.
- Fixed-seed sampling and stop reasons match.
- KV ring wraparound and continuation pass.
- All error, cancellation, and shutdown paths are bounded.
- Rust API requires no changes to switch worker languages.

## Phase 10: Rust prefill and Apple tensor paths

Port the production behavior before introducing new policies:

- 128-token chunks
- projection- and shape-specific GEMV/QMM selection
- bounded reusable scratch
- staged affine INT4 Metal Performance Primitives path
- batched routed MoE
- final-row-only language-model head
- Apple10 TensorOps full-attention path
- causal-tiled fallback for earlier GPU families

Use capability-based selection, not chip-name assumptions. TensorOps is the
preferred portable MSL primitive where its shape wins, and uses the M5 neural
accelerators for dense compute such as LLM prefill. Single-token decode remains
bandwidth-oriented and keeps custom packed GEMV paths unless measurement proves
otherwise.

### Exit gate

- 121, 527, 1,017, and 3,707-token prefill gates pass.
- Full-attention gates cover 8K, 16K, 32K, and 64K.
- Apple10 TensorOps and earlier-family fallback both pass.
- Direct numerical checks, delta-NLL, top-1/top-k, output quality, and bounded
  RSS pass.
- The Rust path matches or exceeds the accepted Swift performance envelope.

## Phase 11: Metal 4 modernization experiments

After Rust parity, evaluate each feature independently:

- ahead-of-time libraries and Metal 4 compilation workflow
- command allocator reuse
- argument tables
- residency sets
- explicit pass/queue barriers
- parallel command encoding
- Metal I/O queues
- native quantized tensors and scale planes where format-compatible
- Morton-order dispatch for large tensor tiles

Adopt Apple APIs modularly. Each experiment needs a named production control,
representative short/medium/long shapes, capability fallback, correctness and
quality checks, and whole-operation wall time.

Do not promote a change because allocations or an isolated kernel improved.
The acceptance metric is the current end-to-end bottleneck. Use Metal API and
Shader Validation, Metal Debugger, Metal System Trace, and release-mode product
measurements.

## Phase 12: Rust installer and repacker

Port the repacker last. Preserve:

- pinned model revision and accepted source index
- bounded HTTP range downloads
- no full checkpoint or shard on disk
- tile-sized scratch
- direct writes into the final resident/expert layout
- durable range checkpoints
- cancellation and resume
- explicit partial discard
- hashes, receipt binding, advisory locking, and atomic promotion

`hf-hub` may provide Hub metadata and authentication, but the implementation
must retain bounded range transport rather than silently downloading complete
source shards.

### Exit gate

- Rust output is byte-identical to the accepted `.gturbo` contract.
- Interrupted installs resume after verifying completed ranges.
- Damaged ranges are redownloaded.
- Peak scratch remains bounded.
- No full source checkpoint is staged.

## Phase 13: terminal-only cutover

Remove components in this order:

1. Stop building UI targets by default.
2. Remove SwiftNIO after full Rust API parity.
3. Make the Rust worker the default after inference parity.
4. Retain the Swift worker as a named reference backend for a stabilization
   window.
5. Remove Swift CLI after Rust `run`, `inspect`, and `benchmark` parity.
6. Remove Swift repacker after installer parity.
7. Port remaining reference tests and archive a final Swift reference tag.
8. Remove SwiftPM, Mac UI, decode service, and Swift sources.

Final user-facing commands are terminal-only:

```bash
turbofieldfare install --output scratch/gemma4.gturbo
turbofieldfare run --model scratch/gemma4.gturbo --prompt "Hello"
turbofieldfare serve --model scratch/gemma4.gturbo --port 8080
turbofieldfare inspect --model scratch/gemma4.gturbo
turbofieldfare benchmark --model scratch/gemma4.gturbo
```

`serve` may supervise separate API and worker processes while remaining one
operator command.

## Cross-cutting acceptance gates

No component is removed until the replacement meets all applicable gates:

| Dimension | Gate |
| --- | --- |
| HTTP | Complete black-box contract parity |
| Correctness | Kernel/reference and generation fixtures pass |
| Quality | Accepted delta-NLL and top-k agreement gates pass |
| Decode | At least 98% of the frozen Swift baseline |
| Prefill | At least 95% on fallback; no M5 TensorOps regression |
| TTFT | No more than 5% regression |
| Memory | No more than 10% physical-footprint regression |
| I/O | Bounded reads, slots, descriptors, and queues |
| Cancellation | HTTP -> API -> worker -> Metal lifecycle verified |
| Safety | No second model process; no remote unauthenticated exposure |

These are initial migration gates, not permanent performance ceilings. Tighten
them after Rust measurements become stable.

## Learning cadence

Use small, reviewable milestones. Every milestone should contain:

1. one explicit Rust or inference learning objective;
2. one bounded product capability;
3. independent correctness evidence;
4. a named benchmark only when performance can change;
5. documentation of assumptions and rejected alternatives.

Avoid long rewrites that first run after months of work. The intended sequence
of visible wins is:

```text
Rust HTTP mock
-> Rust API + Swift worker
-> full Rust OpenAI surface
-> Rust model/tokenizer inspection
-> first Rust-hosted Metal kernel
-> kernel parity
-> first Rust-generated token
-> Rust decode
-> Rust prefill
-> Rust production worker
-> Rust installer
-> removal of Swift and UI
```

## Immediate milestone

The first implementation PR after this plan should create the Cargo workspace,
IPC DTO crate, mock backend, and Rust implementations of `/health`, `/v1/models`,
and `/v1/chat/completions` with non-streaming plus SSE contract tests. It must
not modify inference runtime behavior or delete existing Swift targets.
