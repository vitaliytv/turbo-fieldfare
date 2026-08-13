# План навчання та міграції на Rust

## Кінцевий результат

Кінцевий продукт працює лише з термінала та реалізований на Rust і Metal
Shading Language:

```text
OpenAI-клієнти
    |
    | HTTP/1.1, JSON, SSE
    v
turbofieldfare-api (Rust)
    |
    | версіонований локальний IPC
    v
turbofieldfare-worker (Rust)
    |
    v
Metal 4 / TensorOps / .gturbo
```

Міграція навмисно гібридна. Спочатку відокремлюємо OpenAI-сумісний сервер і
підключаємо його до наявного inference runtime на Swift/Metal. Потім переносимо
inference worker на Rust, не змінюючи IPC-контракт. Так протягом усього навчання
Rust та inference-технологіям у нас залишатимуться робочий продукт і еталон для
перевірки коректності.

План має два окремі напрями:

1. **Міграція поведінки:** відтворити поточну поведінку на Rust, повторно
   використовуючи формат `.gturbo` і наявні Metal kernels.
2. **Модернізація Metal:** після досягнення паритету оцінити нові Apple API та
   приймати лише зміни, які проходять перевірки коректності, якості, пам'яті й
   повного часу виконання.

Не поєднувати перенесення на іншу мову й оптимізацію в одному етапі.

## Основні обмеження

- Залишити публічний сервер на `127.0.0.1`: він не має віддаленої
  автентифікації чи TLS.
- Завантажувати модель рівно в одному worker-процесі.
- Зберегти авторитетну чергу з одним активним generation у worker. API може
  обмежувати прийом запитів, але не повинен бути єдиною межею серіалізації.
- Не передавати через IPC logits, KV buffers, weights чи інші дані масштабу
  моделі. Передавати лише нормалізовані запити та події text/tool/usage.
- Зберегти обмежене використання пам'яті, expert streaming, integrity checks,
  cancellation, prompt cache і семантику Files/Batch.
- Зберігати еталонну Swift-реалізацію, доки Rust worker не пройде всі перевірки
  паритету.
- Не змінювати наявні `.metal` kernels під час міграції host language. Зміни
  kernels мають бути окремими експериментами.
- Не видаляти UI, Swift server, CLI, repacker або еталонні тести лише для
  спрощення проміжного етапу.

## Базові практики Apple

Підтримуваним покажчиком джерел є [implementation references](IMPLEMENTATION_REFERENCES.md).
Міграція спирається на такі Apple-контракти й практики:

- [Metal Performance Primitives programming guide](https://developer.apple.com/download/files/Metal-Performance-Primitives-Programming-Guide.pdf)
  для TensorOps tiling, cooperative tensors, типів даних, execution scopes та
  alignment.
- [Optimize custom machine learning operations with Metal tensors](https://developer.apple.com/videos/play/wwdc2026/330/)
  для quantized tensors, TensorOps, cooperative reuse і custom attention.
- [Discover Metal 4](https://developer.apple.com/videos/play/wwdc2025/205/)
  для модульного впровадження, command allocation, residency, barriers і
  компіляції shaders.
- [Metal resource loading](https://developer.apple.com/documentation/metal/resource-loading)
  для порівняння Metal I/O queues із наявною обмеженою реалізацією на `pread`.
- [Metal developer tools](https://developer.apple.com/metal/tools/)
  для API/Shader Validation, Metal Debugger і Metal System Trace.

Під час підвищення мінімальної версії macOS/Xcode або додавання шляху для нової
GPU family потрібно повторно перевірити ці джерела й таблиці Metal feature
sets. Новіший API є кандидатом, а не автоматичною заміною виміряного
production-шляху.

## Цільовий Cargo workspace

Workspace зростає поступово; не всі crates мають з'явитися першого дня. Уся
поведінка живе в library crates. Виконувані targets лише читають конфігурацію,
з'єднують реалізації та керують lifecycle процесів.

```text
rust/
  Cargo.toml
  crates/
    inference-core/       доменні request/event/usage та backend/model traits
    protocol/             версіоновані IPC frames і core <-> wire conversion
    ipc-client/           UDS client, reconnect і реалізація backend trait
    openai-api/           Axum routes, OpenAI adapter, SSE, Files і Batch

    gturbo-format/        manifest, layout, indexes і receipt без I/O політики
    gturbo-store/         mmap/pread, integrity та bounded tensor/expert reads
    model-source/         Hub metadata, range transport і source tensors

    gemma4-tokenizer/     tokenizer, chat template і tool parsing Gemma 4
    llm-sampling/         sampling, stop conditions і deterministic RNG
    llm-kv-cache/         загальні типи та політики KV cache
    metal-tensor-types/   shapes, dtypes, layouts і checked byte ranges
    metal-runtime/        безпечний API проєкту поверх objc2-metal
    gemma4-kernels/       Rust host wrappers для MSL kernels Gemma 4
    moe-expert-cache/     slots, LFU planning і bounded expert streaming
    gemma4-inference/     layers, forward, prefill і decode Gemma 4

    runtime/              model lifecycle і реалізація inference-core traits
    worker/               IPC service, queue, cancellation і usage accounting
    repack/               bounded installer/repacker як library API
    test-support/         mock backend, golden fixtures і reusable harnesses

  bins/
    turbofieldfare/        terminal CLI, installer commands і supervisor
    turbofieldfare-api/    тонкий HTTP process: openai-api + ipc-client
    turbofieldfare-worker/ тонкий worker process
  shaders/
    gemma4/                MSL sources та metallib build inputs
  xtask/                   build metallib, fixtures, publish і conformance tasks
```

Назви директорій можуть бути короткими, але package names перед публікацією
мають бути однозначними, наприклад `turbofieldfare-inference-core`,
`turbofieldfare-openai-api` і `gturbo-format`.

Бажаний ациклічний напрям залежностей:

```text
turbofieldfare bin -> ipc-client, repack; запускає API/worker processes
api bin            -> openai-api, ipc-client
worker bin         -> worker, runtime

openai-api -> inference-core
ipc-client -> protocol, inference-core
worker     -> protocol, inference-core
runtime    -> inference-core, gemma4-inference
protocol   -> inference-core

gemma4-inference -> gemma4-tokenizer, gemma4-kernels, llm-sampling,
                    llm-kv-cache, gturbo-store, moe-expert-cache
gemma4-kernels   -> metal-tensor-types, metal-runtime
moe-expert-cache -> gturbo-store
gturbo-store     -> gturbo-format
repack           -> gturbo-format, model-source
```

### Правила меж crates

- `inference-core` не знає про HTTP, IPC, Gemma, Metal або конкретний storage.
- `protocol` містить лише versioned wire contract і conversion; він не містить
  Axum/OpenAI/Swift/Metal types і не є власником бізнес-логіки.
- `openai-api` залежить від trait, а не від Gemma або Metal implementation.
- `ipc-client` є окремою реалізацією backend trait; HTTP crate не знає, чи
  backend локальний, IPC або тестовий.
- `gturbo-format` не виконує network чи Metal I/O; `gturbo-store` не знає про
  HTTP або OpenAI.
- `metal-runtime` не знає про Gemma; model-specific pipelines залишаються в
  `gemma4-kernels`.
- `gemma4-inference` не відкриває sockets і не формує OpenAI responses.
- `MockBackend` і test fixtures належать `test-support`, а не production crates.
- Feature flags вмикають лише необов'язкові інтеграції; вони не створюють дві
  різні семантики одного API.
- Заборонені циклічні залежності та імпорт із executable crate у library crate.

### Публікація і повторне використання

Crates поділяються не за принципом «усе опублікувати», а за стабільністю
контракту:

| Група | Crates | Політика |
| --- | --- | --- |
| Публічні першими | `inference-core`, `openai-api`, `protocol`, `gturbo-format` | SemVer, rustdoc, examples, changelog і conformance tests |
| Публічні після паритету | `ipc-client`, `llm-sampling`, `llm-kv-cache`, `metal-tensor-types`, `metal-runtime`, `gturbo-store`, `moe-expert-cache`, `model-source` | Публікувати після стабілізації transport, unsafe та I/O контрактів |
| Model-specific | `gemma4-tokenizer`, `gemma4-kernels`, `gemma4-inference` | Reusable для інших Gemma 4 застосунків, але без обіцянки універсальності |
| Внутрішня композиція | `runtime`, `worker`, `repack`, binaries, `test-support`, `xtask` | Спочатку `publish = false`; публікувати лише за наявності окремого use case |

Публічний crate повинен мати мінімальний standalone example, README, ліцензію,
MSRV policy, задокументовані safety invariants і `cargo package` check. Його
public API не повинен вимагати unpublished path dependency.

## Повторне використання Rust-екосистеми

Точні версії слід зафіксувати в `Cargo.lock`, а оновлення виконувати свідомо.

| Задача | Готові crates | Наш шар |
| --- | --- | --- |
| Async runtime | `tokio` | binaries, `worker`, `openai-api` |
| HTTP routing | `axum` | `openai-api` |
| HTTP limits, request IDs, tracing | `tower-http` | `openai-api` |
| OpenAI wire types | `openai-protocol` | локальний adapter у `openai-api` |
| Serialization | `serde`, `serde_json` | `protocol`, format/config crates |
| IPC framing | `tokio-util` | `protocol`, `ipc-client` і transport у `worker` |
| Cancellation і streams | `tokio-util`, `tokio-stream`, `futures-util` | `inference-core`, API і worker |
| CLI | `clap` | тонкий `turbofieldfare` binary |
| Errors і diagnostics | `thiserror`, `tracing`, `tracing-subscriber` | усі межі компонентів |
| Metal bindings | `objc2`, `objc2-foundation`, `objc2-metal` | `metal-runtime` |
| FP16/BF16 і POD data | `half`, `bytemuck` | `metal-tensor-types`, kernels |
| Memory maps і POSIX I/O | `memmap2`, `rustix` | `gturbo-store` |
| Tokenization і templates | `tokenizers`, `minijinja` | `gemma4-tokenizer` |
| Hub/range transport | `hf-hub`, `reqwest` | `model-source` |
| Model sources | `safetensors` | `model-source`, де це спрощує parsing |
| Integrity | `sha2` | `gturbo-format`, `gturbo-store`, `repack` |
| Tests і microbenchmarks | `proptest`, `criterion` | `test-support`, crate-local benches |

`openai-protocol` скорочує роботу з wire types, але не замінює специфічну для
TurboFieldfare валідацію. Локальними залишаються правила для model IDs, `n=1`,
sampling ranges, unsupported fields, tool choices, адаптації Gemma schema,
історичних tool calls і фактичних context limits.

Старий crate `metal` застарів. Нова Rust-реалізація Metal використовує
`objc2-metal`, а робота з Objective-C lifetime та `unsafe` ізолюється в
`metal-runtime`.

## Етап 0: зафіксувати еталон

### Навчальні цілі

- Розрізняти точну ідентичність результату, числову похибку, паритет якості
  моделі та HTTP-сумісність на рівні продукту.
- Вивчити поточні контракти `.gturbo`, prefill, decode, MoE streaming і
  вимірювань до початку перенесення.

### Результати

- Визначені Swift reference commit і hash model manifest.
- Black-box HTTP fixtures для routes, errors, SSE, tools, Files і Batch.
- Fixtures для tokenizer і rendered prompts.
- Fixtures input/output для kernels і незалежні FP32 references.
- Generation fixtures із фіксованим seed.
- Зафіксований performance baseline за чинним community benchmark protocol.

Фіксувати commit, hardware, RAM, версії macOS/Swift/Rust/Xcode, точні команди,
exit codes, timing footers, energy mode і всі відхилення від протоколу.

### Критерій завершення

Один і той самий набір тестів зовнішнього контракту запускається для довільного
base URL, а еталонний benchmark повторюється без редагування source code.

## Етап 1: Rust OpenAI server із mock backend

Почати лише з:

```text
inference-core
protocol
openai-api
test-support
turbofieldfare bin
```

Bootstrap може тимчасово початися з `protocol`, `openai-api` і CLI, але до
завершення етапу доменні generation types і `InferenceBackend` мають бути
винесені в `inference-core`, а mock — у `test-support`. Це не дозволяє
початковій структурі випадково стати постійним монолітом.

Реалізувати:

- `GET /health`
- `GET /v1/models`
- `POST /v1/chat/completions`
- non-streaming responses
- SSE chunks, heartbeat, usage chunk і `[DONE]`
- OpenAI error envelopes
- ліміт звичайного request body 1 MiB
- request cancellation та обмежений backpressure
- структуроване логування request і phase

Mock backend з `test-support` генерує детерміновані події `prepared`, `content`,
`tool_call`, `completed` і `failed`. Він має дозволяти тестувати slow clients і
cancellation без моделі.

### Навчальні цілі

- Ownership, enums, traits і error handling у Rust.
- Tokio tasks, channels, cancellation і stream lifetimes.
- Axum extractors/responses та SSE backpressure.
- Сумісність Serde і protocol-oriented testing.

### Критерій завершення

- `cargo fmt --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`
- HTTP contract fixtures проходять для реалізованих routes.
- Від'єднання streaming client скасовує mock generation.
- Повільні clients не спричиняють необмежену буферизацію.
- `openai-api` не залежить від mock, concrete model, IPC або Metal.
- `cargo doc` і standalone mock-server example збираються без Swift/Metal.

## Етап 2: версіонований IPC v1

Використати Unix domain socket із префіксом довжини фіксованого розміру та JSON
payload. JSON робить першу версію протоколу зручною для перевірки зі Swift і
Rust, а framing усуває неоднозначність newline.

Кожен frame містить `protocol_version`, `type` і, де доречно, `request_id`.
Початкові messages:

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

`ready` оголошує capabilities worker: model ID, maximum context, tool support,
prompt-cache mode та maximum concurrency. Протокол містить явні категорії
queue-full, worker-busy, invalid-request, model-error, cancelled і
internal-error.

`inference-core` залишається власником неверсіонованих доменних типів.
`protocol` володіє лише wire frames v1 і явними conversions. Це дозволяє
розвивати in-process backend API та IPC versioning незалежно.

`ipc-client` реалізує `InferenceBackend` поверх Unix socket, correlation IDs,
reconnect і cancellation. `worker` реалізує серверний transport та викликає
переданий йому backend. Тому ні `openai-api`, ні `worker` не залежать від
конкретної Gemma/Metal реалізації.

### Межа відповідальності

Rust API відповідає за:

- HTTP і OpenAI wire compatibility
- загальну валідацію OpenAI fields
- SSE і response envelopes
- request IDs, body limits, Files і Batch

Inference worker відповідає за:

- tokenizer і Gemma chat template
- Gemma-specific tool-schema validation
- фактичну валідацію token context
- generation queue і prompt/KV cache
- sampling, inference, Metal та usage accounting

### Критерій завершення

- Проходять Rust-тести protocol round-trip і malformed frames.
- Swift і Rust декодують однакові golden frames.
- Невідомі protocol versions відхиляються до початку generation.
- Розміри frames і queues обмежені.
- Cancellation ідемпотентний і пов'язаний із request ID.

## Етап 3: Swift inference worker

Створити terminal-only executable `TurboFieldfareWorker` поверх наявного
`ServerModelSession`. Він не відкриває TCP port і не форматує OpenAI responses.

Послідовність запуску:

1. Bind або connect до налаштованого Unix socket.
2. Завантажити й перевірити одну `.gturbo` model.
3. Опублікувати `ready` лише після готовності model і tokenizer.
4. Серіалізувати весь реальний inference через worker coordinator.
5. Стрімити events і виконувати cancellation.
6. Коректно завершуватися за signal від supervisor-власника.

### Критерій завершення

- Rust API разом зі Swift worker проходить HTTP contract suite старого Swift
  server.
- Text, Unicode, tool calls, finish reasons, errors і usage збігаються.
- Interactive і Batch робота використовують одну авторитетну worker queue.
- Падіння worker перетворюється на контрольований HTTP `503`.
- Перезапуск API може відновити з'єднання без завантаження другої моделі.
- Регресія TTFT не перевищує 5%.
- Decode throughput становить щонайменше 98% від in-process Swift baseline.
- IPC memory залишається обмеженою для довгих responses і slow clients.

На цьому етапі Rust стає основним OpenAI-сумісним server. SwiftNIO server
залишається еталоном до повного паритету Files/Batch і failure behavior.

## Етап 4: перенести Files і Batch на Rust

Реалізувати решту API surface в `openai-api`:

- multipart file upload із явним лімітом 200 MiB
- file list, status, content і delete
- JSONL validation
- Batch create, list, status, cancel, expiry і pagination
- output та error JSONL files
- поточні metadata і compatibility limits

Batch надсилає звичайні generation requests через IPC і ніколи не обходить
межу серіалізації worker.

### Критерій завершення

- Повний black-box HTTP contract suite проходить із Rust.
- Batch status transitions і result/error JSONL збігаються.
- Cancellation правильно доходить до queued та active work.
- Семантика server restart явно визначена й протестована.
- SwiftNIO більше не потрібен у production launch path.

## Етап 5: Rust-шар `.gturbo` і tokenizer

Роботу розділити між `gturbo-format`, `gturbo-store` і `gemma4-tokenizer`.
Format crate описує bytes і layout, store реалізує bounded reads та integrity,
а tokenizer містить лише model-specific text contract.

Перенести без Metal execution:

- parsing manifest і verified-install receipt
- валідацію architecture та quantization
- resident index і packed-expert layout
- file sizes, hashes, offsets, strides і alignment
- load, encode, decode та streaming decode tokenizer
- pinned Jinja chat template
- parsing structured assistant/tool-call

Не змінювати `.gturbo`. Одночасна зміна model format забере можливість
незалежно порівнювати host-реалізації.

### Критерій завершення

- Інтерпретація manifest і layout точно збігається зі Swift.
- Token IDs і rendered prompts збігаються з golden fixtures.
- Tool-call structures збігаються.
- Corrupt, truncated, misaligned або incompatible models безпечно відхиляються.
- Жоден test або loader не розміщує tensor масштабу моделі в Rust heap.
- `gturbo-format` має standalone inspect example без Metal і inference.
- `gemma4-tokenizer` тестується без model weights і GPU.

## Етап 6: фундамент Metal на Rust

Спочатку створити `metal-tensor-types` без Objective-C залежностей, потім
невеликий безпечний API проєкту `metal-runtime` поверх `objc2-metal` для:

- device і capability discovery
- command queues, command buffers і compute encoders
- buffers і no-copy ownership
- створення library/function/pipeline
- function constants
- dispatch geometry
- completion, error propagation і synchronization
- labels і diagnostics

Роботу з Objective-C object lifetime і весь неминучий `unsafe` ізолювати в
цьому crate. Для кожного `unsafe` block документувати інваріанти ownership,
alignment, lifetime і synchronization.

Навчальні вправи мають пройти від vector addition через buffer round trips,
function constants і pipeline caching до error/cancellation handling, перш ніж
торкатися kernels моделі.

Production builds постачають попередньо скомпільований `.metallib`. Runtime
source compilation може залишитися явним development fallback. До цього етапу
потрібно встановити й зафіксувати Xcode Metal Toolchain.

### Критерій завершення

- Metal API і Shader Validation не показують помилок.
- CPU reference tests проходять.
- Звільнення resources не може змагатися з GPU work у виконанні.
- Помилки pipeline compilation і cache спостережувані.
- Release startup не потребує runtime MSL compilation.
- CPU-only tests для shapes/layouts не лінкують Metal framework.
- `metal-runtime` не імпортує Gemma-specific constants або pipelines.

## Етап 7: перенести host wrappers для kernels

У `gemma4-kernels` повторно використати наявний MSL і замінити лише Swift host
orchestration.
Рекомендований порядок:

1. embedding lookup
2. RMSNorm
3. RoPE
4. INT8 affine GEMV
5. INT4 affine GEMV
6. logit softcap і sampling
7. attention
8. fused QKV paths
9. shared expert
10. routed MoE
11. fused layer tail і language-model head

Де можливо, порівнювати три незалежні шляхи:

```text
CPU FP32 reference
Swift host + production MSL
Rust host + production MSL
```

Незмінені kernels та inputs мають давати точний результат або вкладатися у
встановлену похибку. Kernels із переставленими TensorOps потребують прямого
числового еталона та перевірок якості моделі, а не тотожності одному порядку
reduction.

## Етап 8: пам'ять моделі та expert streaming

Розмістити загальний bounded storage у `gturbo-store`, а slot planning і
MoE-специфічний lifecycle — у `moe-expert-cache`.

Перенести:

- read-only mapping спільних weights
- no-copy Metal buffer wrapping
- aligned expert-slot allocation
- positional `pread`
- lazy layer opening і verification
- LFU cache на 16 slots із recency tie-break
- parallel bounded reads
- ownership slot до завершення всіх GPU consumers
- планування cached hits і missing experts

Спочатку відтворити поточний шлях `pread`. `MTLIOCommandQueue` оцінювати лише як
окремий control/candidate experiment: наявність API не доводить переваги для
малих, динамічно вибраних MoE experts.

### Критерій завершення

- Slot bytes і cache plans збігаються зі Swift fixtures.
- Slot не використовується повторно до GPU completion.
- Cancellation не залишає slot у невизначеному стані.
- Cold і warm I/O вимірюються окремо.
- Physical footprint і вплив file cache звітуються окремо.

## Етап 9: Rust decode worker

Decode model збирається в `gemma4-inference`; `runtime` адаптує її до
`inference-core`, а `worker` додає IPC, авторитетну чергу і lifecycle. Жоден із
цих шарів не дублює OpenAI validation.

Перенести:

- model ownership і runtime configuration
- FP16 sliding-window та full-attention KV caches
- forward loop із 30 layers
- планування `cb1 -> CPU top-8 -> expert I/O -> cb2`
- overlap shared-expert/read
- tied head, sampling, stops і detokenization
- prompt continuation і cancellation

Перший коректний end-to-end milestone може виконувати prefill token-by-token
через decode path. Це навмисно повільно, але ізолює коректність decode до
додавання складності chunked prefill.

Rust worker використовує той самий IPC v1 contract:

```bash
turbofieldfare serve --worker swift
turbofieldfare serve --worker rust
```

### Критерій завершення

- Проходить паритет first token і fixed prefix там, де operation order
  збігається.
- Fixed-seed sampling і stop reasons збігаються.
- KV ring wraparound і continuation проходять.
- Усі error, cancellation і shutdown paths обмежені.
- Для зміни мови worker не потрібно змінювати Rust API.

## Етап 10: Rust prefill і Apple tensor paths

Спочатку перенести production behavior без нових політик:

- chunks по 128 tokens
- вибір GEMV/QMM залежно від projection і shape
- обмежений reusable scratch
- staged affine INT4 Metal Performance Primitives path
- batched routed MoE
- language-model head лише для фінального row
- Apple10 TensorOps full-attention path
- causal-tiled fallback для попередніх GPU families

Вибір має залежати від capabilities, а не від назви chip. TensorOps є бажаним
portable MSL primitive там, де виграє його shape, і використовує neural
accelerators M5 для dense compute на кшталт LLM prefill. Single-token decode
залишається bandwidth-oriented і зберігає custom packed GEMV paths, доки
вимірювання не доведуть перевагу іншого підходу.

### Критерій завершення

- Проходять prefill gates для 121, 527, 1 017 і 3 707 tokens.
- Full-attention gates покривають 8K, 16K, 32K і 64K.
- Проходять і Apple10 TensorOps, і fallback для попередніх families.
- Проходять direct numerical checks, delta-NLL, top-1/top-k, output quality та
  bounded RSS.
- Rust path відповідає прийнятому Swift performance envelope або перевищує
  його.

## Етап 11: експерименти з модернізації Metal 4

Після паритету Rust незалежно оцінити:

- ahead-of-time libraries і Metal 4 compilation workflow
- command allocator reuse
- argument tables
- residency sets
- explicit pass/queue barriers
- parallel command encoding
- Metal I/O queues
- native quantized tensors і scale planes, де вони сумісні з format
- Morton-order dispatch для великих tensor tiles

Apple API впроваджувати модульно. Кожен експеримент потребує визначеного
production control, репрезентативних short/medium/long shapes, capability
fallback, перевірок коректності та якості й повного wall time операції.

Не приймати зміну лише через покращення allocations або isolated kernel.
Критерієм є поточне end-to-end bottleneck. Використовувати Metal API and Shader
Validation, Metal Debugger, Metal System Trace і release-mode вимірювання
продукту.

## Етап 12: Rust installer і repacker

Переносити repacker останнім. `model-source` відповідає за source metadata і
bounded range transport, `repack` — за conversion/checkpoint policy, а
`gturbo-format` — за target layout. Зберегти:

- pinned model revision і accepted source index
- bounded HTTP range downloads
- відсутність повного checkpoint або shard на disk
- tile-sized scratch
- прямий запис у фінальний resident/expert layout
- durable range checkpoints
- cancellation і resume
- explicit partial discard
- hashes, receipt binding, advisory locking і atomic promotion

`hf-hub` може надати Hub metadata та authentication, але реалізація має
зберегти bounded range transport, а не непомітно завантажувати повні source
shards.

### Критерій завершення

- Rust output побайтово ідентичний прийнятому контракту `.gturbo`.
- Перервані installs продовжуються після перевірки завершених ranges.
- Пошкоджені ranges завантажуються повторно.
- Peak scratch залишається обмеженим.
- Повний source checkpoint не створюється.

## Етап 13: перехід до terminal-only продукту

Видаляти компоненти в такому порядку:

1. Припинити збирати UI targets за замовчуванням.
2. Видалити SwiftNIO після повного паритету Rust API.
3. Зробити Rust worker типовим після досягнення inference parity.
4. Зберігати Swift worker як іменований reference backend протягом
   stabilization window.
5. Видалити Swift CLI після паритету Rust-команд `run`, `inspect` і `benchmark`.
6. Видалити Swift repacker після паритету installer.
7. Перенести решту reference tests і створити фінальний Swift reference tag.
8. Видалити SwiftPM, Mac UI, decode service і Swift sources.

Фінальні команди користувача працюють лише в терміналі:

```bash
turbofieldfare install --output scratch/gemma4.gturbo
turbofieldfare run --model scratch/gemma4.gturbo --prompt "Hello"
turbofieldfare serve --model scratch/gemma4.gturbo --port 8080
turbofieldfare inspect --model scratch/gemma4.gturbo
turbofieldfare benchmark --model scratch/gemma4.gturbo
```

`serve` може керувати окремими API і worker processes, залишаючись однією
командою для оператора.

Усі три binaries залишаються тонкими: CLI лише керує командами/processes, API
binary компонує HTTP з IPC client, а worker binary компонує IPC service з
runtime. Інші проєкти можуть зібрати власний server, worker або embedded runtime
із тих самих library crates.

## Наскрізні приймальні критерії

Компонент не видаляється, доки заміна не пройде всі відповідні перевірки:

| Вимір | Критерій |
| --- | --- |
| HTTP | Повний паритет black-box contract |
| Коректність | Проходять kernel/reference і generation fixtures |
| Якість | Проходять прийняті gates для delta-NLL і top-k agreement |
| Decode | Не менше 98% зафіксованого Swift baseline |
| Prefill | Не менше 95% для fallback; без регресії M5 TensorOps |
| TTFT | Регресія не більше 5% |
| Пам'ять | Регресія physical footprint не більше 10% |
| I/O | Обмежені reads, slots, descriptors і queues |
| Cancellation | Перевірено lifecycle HTTP -> API -> worker -> Metal |
| Безпека | Немає другого model process і віддаленого доступу без auth |
| Архітектура | Немає циклів; executable crates не містять доменної логіки |
| Повторне використання | Публічні crates мають examples і збираються поза product binary |
| Пакування | `cargo package` проходить для кожного кандидата на публікацію |
| Сумісність | Wire protocol, public Rust API і `.gturbo` versioned незалежно |

Це початкові migration gates, а не постійні межі продуктивності. Після
стабілізації вимірювань Rust їх слід посилити.

## Ритм навчання

Працювати малими етапами, зручними для review. Кожен етап має містити:

1. одну явну навчальну ціль щодо Rust або inference;
2. одну обмежену можливість продукту;
3. незалежні докази коректності;
4. визначений benchmark лише тоді, коли може змінитися performance;
5. документацію припущень і відхилених альтернатив.
6. мінімальний reusable API або чітке обґрунтування `publish = false`.

Уникати великих переписувань, які вперше запускаються через місяці роботи.
Послідовність видимих результатів:

```text
Rust HTTP mock
-> Rust API + Swift worker
-> повний OpenAI API на Rust
-> перевірка model/tokenizer на Rust
-> перший Metal kernel із Rust host
-> паритет kernels
-> перший token, згенерований Rust
-> Rust decode
-> Rust prefill
-> production worker на Rust
-> installer на Rust
-> видалення Swift і UI
```

## Найближчий етап

Поточний Phase 1 bootstrap створює Cargo workspace, IPC/domain DTO, mock backend
і Rust-реалізації `/health`, `/v1/models` та `/v1/chat/completions` із
non-streaming і SSE contract tests. Перед початком IPC integration межі потрібно
довести до цільового стану цього плану:

1. створити `inference-core` і перенести туди backend trait та доменні події;
2. перенести наявні доменні типи з `protocol` і зарезервувати його лише для
   versioned wire frames та conversions етапу 2;
3. перенести mock backend у `test-support`;
4. зробити CLI тонким composition root;
5. додати standalone example і перевірку документації для `openai-api`.

Цей крок не змінює inference runtime, IPC integration чи наявні Swift targets.
Після нього Phase 2 може додати transport, не ламаючи публічний API server або
model-specific crates.
