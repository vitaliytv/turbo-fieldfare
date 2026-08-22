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
Metal 4 / TensorOps / SSD-streaming MoE / .gturbo
```

Міграція навмисно гібридна. Спочатку відокремлюємо OpenAI-сумісний сервер і
підключаємо його до наявного inference runtime на Swift/Metal. Потім переносимо
inference worker на Rust, не змінюючи IPC-контракт. Так протягом усього навчання
Rust та inference-технологіям у нас залишатимуться робочий продукт і еталон для
перевірки коректності.

План має два окремі напрями:

1. **Міграція поведінки:** відтворити поточну поведінку на Rust, повторно
   використовуючи формат `.gturbo` і наявні Metal kernels для Gemma 4 та Qwen
   3.6, але не вбудовувати жодну model family у загальні шари.
2. **Модернізація Metal:** після досягнення паритету оцінити нові Apple API та
   приймати лише зміни, які проходять перевірки коректності, якості, пам'яті й
   повного часу виконання.

Не поєднувати перенесення на іншу мову й оптимізацію в одному етапі.

Ціль проєкту — не універсальний runtime для будь-якого Transformer. Це
перевикористовуваний runtime для MoE-моделей, у яких resident tensors і
вибірково прочитані з SSD routed experts виконуються через Metal. Gemma 4
26B-A4B і Qwen 3.6 35B-A3B є першими двома architecture adapters, які доводять
межі спільного ядра.

## Основні обмеження

- За замовчуванням сервер bind-иться на `127.0.0.1` і не потребує auth/TLS.
  General `--host` є цільовою можливістю продукту, але non-loopback bind
  fail-closed без окремої remote-security конфігурації.
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
- Loopback є єдиним production режимом ранніх фаз. Пізніше general `--host`
  дозволяє LAN/remote bind як окремий opt-in transport із явною політикою
  авторизації, TLS/identity та bounded admission; `--host 0.0.0.0` не є
  коротким шляхом для вимкнення цих перевірок.
- Сервер і worker ніколи не виконують tools: вони лише повертають structured
  tool calls. Tool execution, мережеві credentials, policy та повторний
  tool-result turn назавжди залишаються за клієнтом.
- До Rust worker ранні фази не стандартизують runtime controls або capability
  negotiation. API передає лише мінімальний validated generation request;
  canonical config snapshot з'явиться разом із `moe-runtime`, де його можна
  перевірити проти реальних Gemma/Qwen capabilities.

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

### Платформена межа

Фінальний Rust/MoE продукт підтримує лише Apple Silicon на macOS 26+ з
актуальним Xcode Metal toolchain. Це єдина production і CI target matrix для
API, format, Metal 4/TensorOps, benchmarks та release gates.

macOS 15 fallback kernels, Metal 3.2 compatibility і macOS 14 не входять у
міграційний scope. На непідтримуваній платформі CLI має завершитися з явною
діагностикою до model load, без часткового запуску. Усередині підтримуваної
платформи pipeline як і раніше обирається за capability discovery, а не за
назвою chip.

## Що беремо з upstream PR №105

[Upstream PR №105](https://github.com/drumih/turbo-fieldfare/pull/105) є
рухомою гілкою, тому для аналізу плану зафіксовано snapshot `ffef17f`. На цьому
snapshot PR має 457 змінених файлів і окремий Swift target `NVMAIFormat` з
контрактами `GTurboFormatV1`, manifest, resident index і packed-expert layout.
Він також містить Qwen 3.6, 4/6/8-bit repack, hybrid full-attention та
Gated-DeltaNet layers і необов'язковий MTP sidecar.

Цей PR підтверджує такі рішення:

- `.gturbo` залишається runtime-форматом для resident weights і bounded
  `pread` routed experts; GGUF або Safetensors не замінюють його в runtime.
- Safetensors є одним із source checkpoint adapters для repack, а не
  залежністю model loader.
- GTurbo V1 structural codecs треба виділити в незалежний crate без Metal,
  tokenizer, HTTP або конкретної model family.
- Quantization описується по ролях tensorів, а не одним глобальним `int4`.
- Architecture family, tensor-name contract, layer graph, chat dialect і
  family-specific kernels мають обиратися одним adapter/registry механізмом.
- Qwen MTP — sidecar capability конкретної family, а не обов'язок загального
  inference API.

Не копіювати структуру PR механічно. Поточний head PR є Qwen-only і містить
Qwen defaults у runtime types. У Rust-проєкті не можна будувати «generic» core
як одну велику `ArchConfig` із дедалі більшою кількістю family flags або
визначати family лише за shape. Спільне ядро володіє структурними гарантіями;
семантику manifest, tensor catalog і layer graph повністю перевіряє вибраний
architecture adapter.

## Висновки з усіх відкритих upstream PR та issues

Станом на 2026-08-13 переглянуті 29 відкритих PR і 24 відкриті issues upstream.
Нижче — тільки ті висновки, які треба закласти до Metal migration; це не
означає прийняття кожного upstream implementation.

| Ризик або потреба | Джерела | Раннє рішення в плані |
| --- | --- | --- |
| Дублікати/пошкодження long generation, незавершені SSE | #112, #70, #84, PR #107 | sequence-numbered events, lossless UTF-8 assembler, exactly-once terminal event, cross-process deterministic replay і long-generation soak fixtures |
| Cache miss після повторного tool call | #73, PR #125 | typed cache identity: rendered-prefix digest, template/dialect ID, model/config generation; reason-coded hit/miss telemetry, а не пошук неоднозначної subsequence |
| Tool schemas, repeated system messages, JSON output | #103, PR #26, PR #107 | рання нормалізація OpenAI input у API та family parser; `GenerationConstraint` capability відкладається до Rust worker |
| Реальні runtime controls і профілювання | PR #123, PR #119, PR #53 | відкласти `RuntimeConfig` і full capabilities до Rust worker; ранні фази фіксують лише telemetry envelope та мінімальний request contract |
| Друга/третя MoE family, 4/6/8-bit, long context | #44, #54, #102, PR #29, PR #105 | role-based quant capabilities, architecture registry, separate KV/linear state, per-family benchmark matrix і explicit resource estimate до load |
| Installer trust, QAT/custom repos та CDN auth | #52, #109, PR #85, PR #98, PR #115 | на етапах міграції лише curated pinned descriptors; redirect allowlist, secret redaction, receipt provenance та bounded range-transfer conformance tests |
| Startup/worker availability | #25, PR #88, PR #126 | supervisor owns worker; readiness is socket handshake, never a guessed PID; bounded retry/backoff and typed unavailable state, without second model process |
| Compatibility floor | #19, PR #32, PR #110, #121 | macOS 26+ only; upstream macOS 15 fallback не входить у Rust migration scope, unsupported host fail-fast до model load |
| Remote access і external tools | #120, PR #22, PR #79, #122 | loopback default; general `--host` як окремий fail-closed security milestone; server-side tool runner поза scope |
| Files/Batch, multi-turn, media | PR #57, #74, #51, #11, #18, #9 | durable job state and prompt history contracts belong above inference; migration remains text-only; future media is a separately designed bounded `MediaRef`, never an untyped HTTP blob passed to runtime |

UI-only PRs (#91, #99, #114, #68, #15) і app packaging issues (#48, #86,
#104) не змінюють Rust terminal/MoE boundaries. Їх можна переосмислити після
terminal-only migration як незалежні clients над OpenAI API.

### Рішення щодо media/vision

Міграція до Rust/MoE залишається **text-only**. Підтримка зображень не є
неявним розширенням OpenAI DTO або IPC, а окремою майбутньою capability після
появи VLM-family, її memory model і benchmark-профілю.

Тоді API може прийняти лише bounded `MediaRef`: opaque ID, MIME type,
контрольні limits для bytes і dimensions, checksum, lifetime/ownership та
явно оголошену modality capability архітектури. Base64 або інші binary blobs
не переносяться в IPC, tokenizer чи Metal runtime. Це зберігає сьогоднішній
text inference contract малим і не прив'язує MoE runtime до транспорту або
сховища media.

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
    job-store/            storage traits, IDs, state transitions, expiry policy
    job-store-sqlite/     локальна durable SQLite реалізація job-store
    artifact-store/       content-addressed artifact trait і retention contract
    artifact-store-fs/    managed data directory, hashes і atomic publication
    runtime-config/       Rust-worker config snapshots і family capabilities
    runtime-observability/ versioned metrics/events, no-op by default

    moe-core/             architecture/executor traits, topology і tensor roles
    gturbo-format/        manifest, layout, indexes і receipt без I/O політики
    gturbo-store/         mmap/pread, integrity та bounded tensor/expert reads
    model-source/         Hub metadata, range transport і source tensors

    llm-prompt/           tokenizer loading, template і prompt-dialect traits
    llm-sampling/         sampling, stop conditions і deterministic RNG
    llm-kv-cache/         загальні типи та політики KV cache
    metal-tensor-types/   shapes, dtypes, layouts і checked byte ranges
    metal-runtime/        безпечний API проєкту поверх objc2-metal
    moe-metal-kernels/    спільні quant/GEMV/router/routed-MoE primitives
    moe-expert-cache/     slots, LFU planning і bounded expert streaming

    gemma4-arch/          Gemma schema, tensor catalog, prompt і layer graph
    gemma4-metal/         лише Gemma-specific Metal pipelines
    qwen36-arch/          Qwen schema, ChatML, hybrid graph і optional MTP
    qwen36-metal/         Qwen Gated-DeltaNet, gates та інші family pipelines

    moe-runtime/          model lifecycle, registry і inference-core backend
    worker/               IPC service, queue, cancellation і usage accounting
    repack-core/          bounded installer/checkpoint/writer orchestration
    gemma4-repack/        Safetensors -> GTurbo mapping для Gemma 4
    qwen36-repack/        Safetensors -> GTurbo mapping для Qwen 3.6
    test-support/         mock backend, golden fixtures і reusable harnesses

  bins/
    turbofieldfare/        terminal CLI, installer commands і supervisor
    turbofieldfare-api/    тонкий HTTP process: openai-api + ipc-client
    turbofieldfare-worker/ worker + runtime + зареєстровані architectures
  shaders/
    common/                спільні MoE/quant/attention MSL primitives
    gemma4/                Gemma-specific MSL sources
    qwen36/                Qwen-specific MSL sources
  xtask/                   build metallib, fixtures, publish і conformance tasks
```

Назви директорій можуть бути короткими, але package names перед публікацією
мають бути однозначними, наприклад `turbofieldfare-inference-core`,
`turbofieldfare-openai-api` і `gturbo-format`.

Бажаний ациклічний напрям залежностей:

```text
turbofieldfare bin -> ipc-client, repack-core; запускає API/worker processes
api bin            -> openai-api, ipc-client
worker bin         -> worker, moe-runtime, gemma4-metal, qwen36-metal

openai-api -> inference-core, job-store, artifact-store
job-store-sqlite -> job-store
artifact-store-fs -> artifact-store
ipc-client -> protocol, inference-core
worker     -> protocol, inference-core, runtime-observability
protocol   -> inference-core

moe-runtime -> inference-core, moe-core, runtime-config, runtime-observability,
               gturbo-store, llm-prompt, llm-sampling, llm-kv-cache,
               moe-expert-cache
gemma4-arch -> moe-core, gturbo-format, llm-prompt
qwen36-arch -> moe-core, gturbo-format, llm-prompt
moe-metal-kernels -> metal-tensor-types, metal-runtime
gemma4-metal -> gemma4-arch, moe-metal-kernels, metal-runtime
qwen36-metal -> qwen36-arch, moe-metal-kernels, metal-runtime
moe-expert-cache -> gturbo-store -> gturbo-format
gemma4-repack, qwen36-repack -> repack-core, model-source, gturbo-format
```

### Правила меж crates

- `inference-core` не знає про HTTP, IPC, model family, Metal або storage.
- `runtime-config` з'являється лише разом із Rust worker. Він не читає
  environment або CLI самостійно: parsing належить binaries, а цей crate тільки
  canonicalizes/validates values та capabilities.
- `runtime-observability` не керує lifecycle і має cheap disabled path; його
  event schema versioned та не містить prompts, credentials чи model bytes.
- `moe-core` описує лише стабільні MoE concepts та traits. Нова family не може
  додати до нього прапорець, доки спільний concept не доведено двома adapters.
- `protocol` містить лише versioned wire contract і conversion; він не містить
  Axum/OpenAI/Swift/Metal types і не є власником бізнес-логіки.
- `openai-api` залежить від trait, а не від family або Metal implementation.
- `job-store` містить тільки versioned metadata, global-namespace lifecycle,
  idempotency, state transitions, JSONL references і expiry.
  `job-store-sqlite` — перший terminal-first durable backend; він не зберігає
  model weights, KV cache, logits або runtime payloads і не є залежністю worker.
- `artifact-store` зберігає тільки opaque content-addressed file/JSONL bytes.
  `artifact-store-fs` пише їх у managed data directory через private staging,
  hash verification, `fsync` і atomic rename; SQLite зберігає лише reference
  та global lifecycle metadata. GC прибирає orphaned або expired artifacts
  лише після узгодження з transactional job state.
- Default data root є відносним до current working directory:
  `./.turbofieldfare/` містить SQLite metadata і managed artifacts. Запуск із
  іншої working directory навмисно створює незалежний local instance; macOS
  Application Support і обов'язковий `--data-dir` не входять у першу версію.
- Data root не має exclusive instance lock: кілька processes можуть звертатися
  до нього. `job-store-sqlite` мусить серіалізувати state transitions через
  SQLite transactions, а `artifact-store-fs` — через унікальний staging і
  atomic publish. Підтримувана operational topology — рівно один `serve`
  instance на data root; паралельні instances не блокуються, але не мають
  гарантій для endpoint, worker, queue або performance і не отримують
  спеціальної координації чи global generation lease.
- `RetentionPolicy` розділяє Files і локальну conversation history. Strict
  Files defaults: input з `purpose=batch` спливає через 30 днів, а створені
  сервером output/error з `purpose=batch_output` не мають automatic TTL і
  зберігаються до explicit delete. Batch create може змінити це лише через
  `output_expires_after`. Conversation history не є OpenAI Files resource й
  лишається локальною 7-денною політикою. Expired input стає недоступним до
  physical GC; explicit delete є idempotent і може звільнити artifact раніше.
  Upload `expires_after` зберігає повний Files contract: exact object
  `{ anchor: "created_at", seconds }` визначає індивідуальний `expires_at`
  Batch input; якщо параметр відсутній, діє default 30 днів.
- `ipc-client` є окремою реалізацією backend trait; HTTP crate не знає, чи
  backend локальний, IPC або тестовий.
- `gturbo-format` не виконує network чи Metal I/O; `gturbo-store` не знає про
  HTTP або OpenAI.
- `metal-runtime` не знає про model families; спільні kernels не містять Gemma
  або Qwen tensor names.
- `gemma4-arch` і `qwen36-arch` повністю володіють tensor catalog, semantic
  manifest validation, prompt dialect і layer graph своїх families, але не
  залежать від Metal.
- `gemma4-metal` і `qwen36-metal` реалізують executor factories для відповідних
  CPU-only descriptors; залежність ніколи не спрямована з `*-arch` до Metal.
- `moe-runtime` приймає architecture factories через builder/registry і не
  залежить від конкретного adapter.
- Architecture adapters не відкривають sockets і не формують OpenAI responses.
- `MockBackend` і test fixtures належать `test-support`, а не production crates.
- Feature flags вмикають лише необов'язкові інтеграції; вони не створюють дві
  різні семантики одного API.
- Заборонені циклічні залежності та імпорт із executable crate у library crate.

### Межа GTurbo та architecture adapters

`gturbo-format` реалізує точний on-disk GTurbo V1 structural contract:

- magic і format major/minor;
- безпечні relative paths, file sizes, hashes та install receipt binding;
- resident index із checked ranges, dtype, shape і alignment;
- packed-expert layout із layer files, logical/physical expert mapping,
  per-expert tensor ranges і stride;
- common manifest envelope, quantization slots і повний architecture payload.

Format crate перевіряє overflow, overlap, truncation, path safety та
manifest/layout consistency, але не вирішує, чи Qwen layer graph або Gemma
tensor catalog семантично правильні. Повний architecture payload не можна
мовчки втрачати через Serde unknown fields: він передається вибраному adapter.

Версії незалежні:

```text
GTurbo format version       змінюється через несумісний on-disk layout
architecture_id             gemma4, qwen36, qwen36_mtp, ...
architecture_schema_version змінюється через семантику family payload
quantization capabilities   перевіряються adapter-ом по tensor roles
```

Існуючі Gemma GTurbo V1 та артефакти з PR №105 читаються compatibility
decoders. Якщо старий manifest не має явного `architecture_id`, family можна
визначити лише в legacy adapter з суворою перевіркою всього descriptor; нові
артефакти завжди записують явні ID та schema version. Створення GTurbo V2 не є
частиною міграції, доки V1 може безпечно виразити обидві families.

### Публікація і повторне використання

Crates поділяються не за принципом «усе опублікувати», а за стабільністю
контракту:

| Група | Crates | Політика |
| --- | --- | --- |
| Публічні першими | `inference-core`, `openai-api`, `protocol`, `gturbo-format`, `moe-core` | SemVer, rustdoc, examples, changelog і conformance tests |
| Публічні після паритету | `runtime-config`, `ipc-client`, `llm-prompt`, `llm-sampling`, `llm-kv-cache`, `metal-tensor-types`, `metal-runtime`, `moe-metal-kernels`, `gturbo-store`, `moe-expert-cache`, `model-source` | Публікувати після стабілізації runtime, transport, unsafe та I/O контрактів |
| Model-specific | `gemma4-arch`, `gemma4-metal`, `gemma4-repack`, `qwen36-arch`, `qwen36-metal`, `qwen36-repack` | Reusable family adapters без обіцянки універсальності |
| Внутрішня композиція | `runtime-observability`, `moe-runtime`, `worker`, `repack-core`, binaries, `test-support`, `xtask` | Спочатку `publish = false`; публікувати лише за наявності окремого use case |

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
| Tokenization і templates | `tokenizers`, `minijinja` | `llm-prompt` + family adapters |
| Hub/range transport | `hf-hub`, `reqwest` | `model-source` |
| Model sources | `safetensors` | `model-source`, де це спрощує parsing |
| Integrity | `sha2` | `gturbo-format`, `gturbo-store`, `repack-core` |
| Constrained decoding | `llguidance` як optional feature; `xgrammar` лише benchmark control | `constraint-adapter` над sampler/logit mask |
| Tests і microbenchmarks | `proptest`, `criterion` | `test-support`, crate-local benches |

`openai-protocol` скорочує роботу з wire types, але не замінює специфічну для
TurboFieldfare валідацію. Локальними залишаються правила для model IDs, `n=1`,
sampling ranges, unsupported fields, tool choices, family-specific tool
schemas, історичних tool calls і фактичних context limits.

Старий crate `metal` застарів. Нова Rust-реалізація Metal використовує
`objc2-metal`, а робота з Objective-C lifetime та `unsafe` ізолюється в
`metal-runtime`.

### Рішення щодо constrained decoding

До Rust decode parity діє family parser з контрольованим typed failure або
documented text fallback. Після появи Rust sampler додається
`constraint-adapter` з optional feature `llguidance`: він компілює обмеження і
подає allow-mask у sampler, але не залежить від HTTP, Metal чи конкретної family.

`xgrammar` не є production dependency. Його можна інтегрувати лише у
benchmark/conformance harness як контроль для JSON/tool-call grammars на Gemma
і Qwen tokenizers. Рішення про заміну `llguidance` можливе тільки за виміряними
даними: compile latency, decode overhead, allocation/RSS, valid-completion rate
та коректність при cancellation, thinking і long generation.

Перший production constraint profile не приймає довільний JSON Schema. Він
обмежує наш JSON/tool-call envelope: допустимі tool names, JSON arguments і
закриття family tags. Довільний JSON Schema — окремий пізніший capability після
conformance matrix і явних resource limits для schema/grammar.

### Рішення щодо telemetry та profiling

Обрано варіант B: `runtime-observability` визначає стабільний versioned JSON
envelope вже в ранніх API/IPC фазах, але disabled за замовчуванням і не торкається
Metal або model I/O. Ранні події обмежені `request_id`, lifecycle phase,
queue/admission, HTTP/IPC error category, event sequence та cache miss reason.

Rust worker додає реальні decode/prefill, expert-cache, SSD I/O, GPU interval,
RSS і allocation metrics у той самий envelope. Кожне поле має unit, scope,
availability і privacy classification; prompts, tool arguments, URLs, headers,
tokens та model bytes заборонені. `--profile-json` або еквівалент вмикається
явно, а no-op path має окремий overhead benchmark.

## Етап 0: зафіксувати еталон

### Навчальні цілі

- Розрізняти точну ідентичність результату, числову похибку, паритет якості
  моделі та HTTP-сумісність на рівні продукту.
- Вивчити поточні контракти `.gturbo`, prefill, decode, MoE streaming і
  вимірювань до початку перенесення.

### Результати

- Визначені Swift reference commit і hash model manifest.
- Зафіксовані окремі reference snapshots для Gemma 4 та Qwen 3.6; для PR №105
  записано commit, оскільки PR залишається open і може змінитися.
- Black-box HTTP fixtures для routes, errors, SSE, tools, Files і Batch.
- Fixtures для Gemma та Qwen tokenizer, rendered prompts і tool dialects.
- Спільні й family-specific kernel fixtures та незалежні FP32 references.
- Generation fixtures із фіксованим seed для кожної family/quantization.
- Performance baseline за community protocol окремо для кожної перевіреної
  family, quantization, context shape та cache mode.
- Long-generation conformance fixtures: stream output рівно один раз,
  monotonic event sequence, коректний UTF-8, один terminal event і відсутність
  дублювання при cancellation/failure/reconnect.
- Prompt-cache fixtures з repeated identical tool calls, повторними system
  messages та явним reason code для кожного hit/miss.
- Conformance matrix для JSON/tool constraints: every accepted constrained
  output parses under the selected family dialect; no-tools request не може
  відкрити tool-call region.
- Startup fixtures: delayed readiness, worker crash before/after `ready`,
  reconnect і cleanup socket без запуску другого model process.
- Machine-readable telemetry schema і redaction fixtures; ранній envelope
  покриває request/lifecycle/cache, а I/O/GPU/memory fields реєструються як
  unavailable до Rust worker.
- Installer security fixtures: curated pinned source descriptor, redirect
  allowlist, auth-header forwarding only to approved hosts, resume/receipt
  provenance і відсутність secret у logs/errors.

Фіксувати commit, hardware, RAM, версії macOS/Swift/Rust/Xcode, точні команди,
exit codes, timing footers, energy mode і всі відхилення від протоколу.

### Критерій завершення

Один і той самий набір тестів зовнішнього контракту запускається для довільного
base URL, а еталонний benchmark повторюється без редагування source code.
Long-generation replay щонайменше у двох свіжих processes має однаковий output
для greedy/fixed-seed case і має пояснювану event trace для sampling case.

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
- lossless incremental UTF-8 і monotonic `event_seq` у внутрішньому stream;
  HTTP adapter не дублює content під час flush, abort або error
- OpenAI error envelopes
- ліміт звичайного request body 1 MiB
- request cancellation та обмежений backpressure
- структуроване логування request і phase без prompt/secret content

`GET /v1/models` повертає лише active ready worker model generation. Коли
worker не ready, unavailable або не має current descriptor, endpoint повертає
typed `503 model_unavailable`; API не показує empty list, last-known descriptor
або model, яку ще неможливо прийняти у Chat/Batch request.

`GET /health` має readiness semantics: `200` лише коли API process і active
worker ready для generation. Worker unavailable/not-ready повертає `503`, а
health body не маскує цей стан stale model descriptor або historical readiness.

Mock backend з `test-support` генерує детерміновані події `prepared`, `content`,
`tool_call`, `completed` і `failed` з sequence number. Він має дозволяти
тестувати slow clients, cancellation, duplicate-delivery defense, invalid UTF-8
та failure after partial output без моделі.

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
- Один request має рівно один terminal event; content events мають строго
  зростаючу sequence і не повторюються після cancellation/error.
- UTF-8 boundary tests покривають split scalar, tool JSON і heartbeats.
- `openai-api` не залежить від mock, concrete model, IPC або Metal.
- `cargo doc` і standalone mock-server example збираються без Swift/Metal.

## Етап 2: версіонований IPC v1

Використати Unix domain socket із префіксом довжини фіксованого розміру та JSON
payload. JSON робить першу версію протоколу зручною для перевірки зі Swift і
Rust, а framing усуває неоднозначність newline.

Кожен frame містить `protocol_version`, `type` і, де доречно, `request_id`.
Generation events також містять `event_seq`; worker ніколи не перевикористовує
його в межах request. `generate` несе тільки нормалізований мінімальний request,
без CLI defaults, profile names або майбутнього full config snapshot.
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

Ранній `ready` оголошує тільки protocol version, model ID, maximum context,
basic tool support і maximum concurrency. Повний versioned capability descriptor
(architecture ID/schema, modality, per-role quantization, cache modes,
constraints, MTP та runtime-control ranges) є milestone Rust worker.
Протокол містить явні категорії queue-full, worker-busy, invalid-request,
unavailable, model-error, cancelled і internal-error.

`inference-core` залишається власником неверсіонованих доменних типів.
`protocol` володіє лише wire frames v1 і явними conversions. Це дозволяє
розвивати in-process backend API та IPC versioning незалежно.

`ipc-client` реалізує `InferenceBackend` поверх Unix socket, correlation IDs,
reconnect і cancellation. `worker` реалізує серверний transport та викликає
переданий йому backend. Тому ні `openai-api`, ні `worker` не залежать від
конкретної Gemma/Metal реалізації.

До Rust worker JSON/tool constraints залишаються в HTTP/family parser boundary
і не розширюють IPC v1. Разом із Rust sampler з'являється малий versioned
`GenerationConstraint` descriptor (`none`, `json`, `tool-call`), не
implementation grammar. Тоді API перевіряє capability, а worker/family adapter
виконує конкретну grammar; довільна schema лишається unsupported до окремих
performance та correctness gates.

### Межа відповідальності

Rust API відповідає за:

- HTTP і OpenAI wire compatibility
- загальну валідацію OpenAI fields
- SSE і response envelopes
- request IDs, body limits, Files і Batch

Inference worker і вибраний architecture adapter відповідають за:

- tokenizer, family chat template і tool-schema validation
- фактичну валідацію token context
- generation queue і prompt/KV cache
- sampling, inference, Metal та usage accounting

### Критерій завершення

- Проходять Rust-тести protocol round-trip і malformed frames.
- Swift і Rust декодують однакові golden frames.
- Невідомі protocol versions відхиляються до початку generation.
- Розміри frames і queues обмежені.
- Cancellation ідемпотентний і пов'язаний із request ID.
- `event_seq` строго монотонний; API deduplicates repeated IPC event після
  reconnect і не створює другого terminal response.
- Golden frames покривають basic `ready`, unavailable worker, malformed frames
  і restart; expanded capabilities/config/constraints додаються з Rust worker.

## Етап 3: Swift inference worker

Створити terminal-only executable `TurboFieldfareWorker` поверх наявного
`ServerModelSession`. Він не відкриває TCP port і не форматує OpenAI responses.

Послідовність запуску:

1. Bind або connect до налаштованого Unix socket.
2. Завантажити й перевірити одну `.gturbo` model через architecture registry.
3. Опублікувати `ready` лише після готовності model і tokenizer.
4. Серіалізувати весь реальний inference через worker coordinator.
5. Стрімити events і виконувати cancellation.
6. Коректно завершуватися за signal від supervisor-власника.

Supervisor володіє socket path, child process і cleanup. Готовність означає
лише успішний `ready` handshake після model validation, не факт `spawn`, PID,
launchd state або існування stale socket. Один bounded restart/reconnect policy
повідомляє API typed `unavailable`; він ніколи не створює паралельний worker.

Обрано supervisor topology: `turbofieldfare serve` запускає окремі API і worker
processes, але лишається однією командою для оператора. API restart не
перезапускає worker/model; supervisor restart завершує лише процеси, які він
створив. `launchd` не є runtime dependency або source of truth для Rust
terminal product. Підтримується один такий process tree на data root; другий
не блокується, але є непідтримуваним і не ділить worker, socket, queue чи model
з першим.

У підтримуваному instance worker має одну strict FIFO queue для interactive і
Batch generation requests. Admission order не залежить від типу клієнта або
job; priority, aging і GPU micro-batching не входять у parity scope та можуть
з'явитися лише після окремого benchmark і lifecycle design.

Admission є bounded за кількістю requests і safe request-size estimate.
Переповнення не утримує HTTP connection: API повертає typed `queue_full`
(HTTP 429) з documented retry hint, не створюючи worker job. Default capacity
— одна active generation і до 16 queued requests; `--queue-limit` змінює
admission, а не generation parallelism. Більше значення зменшує 429, але може
збільшити wait time і memory pressure.

### Критерій завершення

- Rust API разом зі Swift worker проходить HTTP contract suite старого Swift
  server.
- `GET /v1/models` повертає лише active ready worker generation; без неї має
  typed `503 model_unavailable`, без empty або historical model list.
- `GET /health` повертає `200` лише за API + active worker readiness; worker
  unavailable/not-ready має `503`, без stale readiness/model status.
- Text, Unicode, tool calls, finish reasons, errors і usage збігаються.
- Interactive і Batch робота використовують одну авторитетну worker queue.
- Queue order для interactive і Batch є strict FIFO та покривається fixtures.
- Queue overflow повертає `queue_full`/HTTP 429 без прийняття worker job або
  утримання HTTP connection.
- Падіння worker перетворюється на контрольований HTTP `503`.
- Перезапуск API може відновити з'єднання без завантаження другої моделі.
- Регресія TTFT не перевищує 5%.
- Decode throughput становить щонайменше 98% від in-process Swift baseline.
- IPC memory залишається обмеженою для довгих responses і slow clients.
- Crash до `ready`, crash під generation, stale socket і delayed startup мають
  детермінований API результат та не залишають orphan model process.

На цьому етапі Rust стає основним OpenAI-сумісним server. SwiftNIO server
залишається еталоном до повного паритету Files/Batch і failure behavior.

## Етап 4: перенести Files і Batch на Rust

Реалізувати решту API surface в `openai-api`:

- multipart Batch file upload із strict filename `.jsonl` та лімітом 200 MB
- file retrieve/status, content і delete; `GET /v1/files` не підтримується
- streaming JSONL syntax і required-field validation під час upload
- Batch create, list, status, cancel, expiry і pagination
- canonical OpenAI Batch output та error JSONL envelopes
- поточні metadata і compatibility limits

Supported Files surface приймає uploads лише з `purpose=batch`. Інші purpose
відхиляються до artifact write; сервер не створює generic Files без runtime
semantics. Batch input повинен мати filename `.jsonl` і не перевищувати 200 MB:
розмір перевіряється однопрохідно до atomic publish, а overflow не залишає
видимого або durable artifact. Batch input FileObject зберігає
`purpose=batch`, а output і error JSONL, створені server, мають
`purpose=batch_output` і доступні через звичайні retrieve/content/delete
routes. Input `batch` має strict 30-денний TTL, тоді
як server-generated `batch_output` зберігається до explicit delete; 7-денна
conversation history не є Files artifact. Для input підтримується
`expires_after={anchor:"created_at",seconds}`: він валідовується до artifact
write, обчислює та повертає FileObject `expires_at`; без параметра сервер
повертає default `created_at + 30 days`. `POST /v1/batches` також приймає
`output_expires_after={anchor:"created_at",seconds}`: `seconds` 3 600…2 592
000 задає `expires_at` для створених output/error Files від їхнього власного
`created_at`; відсутність поля лишає їх до explicit delete. Invalid policy
відхиляється до job creation. `GET /v1/files` свідомо не routed і повертає
звичайний endpoint-not-found response без розкриття shared resource namespace.

`GET /v1/batches` є повною cursor-paginated surface над durable Batch jobs:
підтримує тільки query `after` і `limit`; `limit` у межах 1…100, default 20,
а `after` є ID останнього Batch з попередньої сторінки. Відповідь — CursorPage
`{ object: "list", data, first_id, last_id, has_more }` із тими самими Batch
object, що повертають create/retrieve. Pagination не додає tenant isolation:
за прийнятим global namespace remote keys бачать той самий список jobs.

`BatchStatus` є публічним durable state machine: successful flow проходить
`validating → in_progress → finalizing → completed`; job-level failure —
`validating → failed`; deadline — `validating|in_progress|finalizing → expired`;
cancel — `validating|in_progress|finalizing → cancelling → cancelled`.
`created_at` встановлюється при create, `expires_at` — його 24h deadline, а
`in_progress_at`, `finalizing_at`, `completed_at`, `failed_at`, `expired_at`,
`cancelling_at` та `cancelled_at` встановлюються рівно на відповідному
transition, лише один раз і ніколи не переписуються після restart. Усі
create/retrieve/list/cancel responses повертають ці поля з однакової
transactional state.

Batch-level failure повертає `errors={object:"list",data:[BatchError]}`, де
кожен `BatchError` має `code`, `message`, optional `param` та optional input
`line`; не застосовні `param`/`line` серіалізуються як `null`. Усі інші Batch
statuses повертають `errors=null`. Це не заміняє per-record error JSONL:
останній описує individual request failures, тоді як `errors` пояснює лише
неможливість виконати Batch як job.

Підтримується лише `completion_window=24h`; він фіксується під час Batch create
разом із deadline. Після deadline dispatcher припиняє новий dispatch, прибирає
queued records і надсилає cancellation active record. Після terminal
acknowledgement Batch стає `expired`; кожен record без уже durable HTTP result
отримує один canonical error envelope з `response=null` і
`error.code=batch_expired`. Already-produced output/error і counters лишаються
доступними за звичайним retention policy. Довільні completion windows не
входять у першу OpenAI-compatible surface.

Batch `metadata` підтримується повністю: optional map до 16 string pairs,
ключ до 64 characters і value до 512 characters. Валідація виконується на
create, SQLite зберігає canonical map, а кожна Batch відповідь повертає її без
зміни. Metadata не передається worker, не впливає на scheduler і не потрапляє
до telemetry за замовчуванням.

`body.store=false` забороняє створювати local conversation-history record для
цього Batch record. Відсутнє або `true` зберігає його за чинною 7-денною
history policy. Це не змінює обов'язкову durable обробку Batch: input JSONL,
cursor, request counts та output/error artifacts зберігаються за власними
Files/Batch retention contracts; `store` не впливає на scheduler, shared
namespace чи telemetry.

`body.user` не входить до Batch surface до появи окремого principal/audit
contract. Його присутність хоча б в одному record відхиляє весь Batch у create
preflight до job ID/cursor/queue admission: API не зберігає opaque label, не
виводить telemetry identity і не створює неявної authorization або namespace
isolation.

`service_tier` не входить до Batch surface: його присутність хоча б в одному
record відхиляє весь Batch у create preflight до job ID/cursor/queue admission.
Single-worker strict FIFO не має cloud tier, priority, credit чи SLA semantics,
тому API не мапить `auto`, `default` або `flex` у ту саму чергу.

До Rust-worker tokenizer-aware prediction-matching parity `prediction` не
підтримується: його присутність хоча б в одному record відхиляє весь Batch у
create preflight до job ID/cursor/queue admission. API не ігнорує prediction і
не оголошує latency optimization без exact token-level matching contract.

`output_expires_after` є optional Batch-create policy для обох generated
`batch_output` Files. Підтримується тільки exact
`{anchor:"created_at",seconds}` з `seconds` 3 600…2 592 000; anchor прив'язаний
до File creation, не до Batch creation. Valid policy durable зберігається з
job і застосовується до кожного output/error artifact; malformed або
unsupported policy відхиляє Batch до створення ID. Без параметра generated
files не мають automatic expiry.

Для strict supported-surface parity `endpoint` на Batch create може бути лише
`/v1/chat/completions`. Будь-який інший endpoint відхиляється до створення job
з typed unsupported-endpoint error; dispatcher ніколи не приймає завідомо
непідтримуваний Batch та не перетворює його на per-record failures.

Batch є атомарно прив'язаним до active advertised model ID worker. Під час
`POST /v1/batches` API однопрохідно preflight-сканує `body.model` усіх records
проти цього ID; хоча б один missing або інший model відхиляє весь Batch до
створення ID, cursor, queue admission чи worker dispatch. Це не неявна
підстановка моделі й не per-record failure; перевірка лишається bounded та не
розгортає JSONL у RAM.

`POST /v1/batches` вимагає active ready worker з поточним model descriptor.
Якщо readiness або descriptor недоступні, API повертає typed `503
model_unavailable` до Batch ID, cursor, queue admission чи durable metadata;
він не приймає job для відкладеної validation і не довіряє last-known model ID.

Після успішного create Batch durable pin-ить model-generation fingerprint:
public model ID, manifest identity та prompt-dialect/config generation. Перед
кожним dispatch dispatcher звіряє його з active worker. Якщо generation
відрізняється, record не запускається і отримує canonical `model_unavailable`
error; API не виконує його на новому artifact навіть за того самого public model
ID і не змішує outputs різних model generations в одному Batch.

Ранній Batch text-only: `body.messages` кожного record можуть містити лише
підтримуваний text content. Один image, audio, file або інший non-text modality
відхиляє весь Batch у тому ж streaming create preflight до job ID, cursor,
queue admission чи worker dispatch. API не видаляє content parts, не
перетворює їх на text і не відкладає помилку до per-record output; bounded
`MediaRef` стане окремою майбутньою capability.

До cross-family prompt-dialect contract роль `developer` не входить до Batch
`body.messages`: один такий message відхиляє весь Batch у create preflight до
job ID/cursor/queue admission. API не зливає його з `system` і не дозволяє
family adapter-ам неявно змінювати hierarchy інструкцій.

Batch підтримує canonical history із `system`, `user`, `assistant` і `tool`
messages тією ж validation, що interactive Chat. Validator зберігає порядок,
вимагає unique assistant tool-call IDs і exact `tool_call_id` для кожного tool
result; malformed або неузгоджена history завершує лише цей record canonical
per-record error. Family adapter рендерить validated history у власний prompt
dialect без зведення assistant/tool content до user text.

Кілька `system` messages зберігаються як окремі ordered nodes: API не merge-ить
їх через separator і не відкидає дублікати. Family adapter явно рендерить їх у
своєму dialect, а prompt-cache identity походить з canonical rendered prefix,
тож зміна одного system message не може дати cache hit для іншого prompt.

History із незавершеним tool loop не є dispatchable: кожен assistant tool call
мусить мати відповідний `tool` result до наступної generation. Відсутній result
завершує лише цей record canonical per-record error; server/worker не
продовжують generation, не виконують tool і не додають synthetic tool result.

Після canonical prompt render dispatcher exact tokenizer-count перевіряє
history разом із requested completion limit проти active model context window.
Overflow завершує лише цей record canonical `context_length_exceeded` error до
worker generation; API не обрізає oldest messages, system/tool history або
prompt text, а cursor продовжує наступний valid record.

Якщо `max_completion_tokens` відсутній, record може генерувати до всього
залишку active context window після prompt render. Це не означає unbounded
generation: effective limit є `context_window - prompt_tokens`, а Batch
deadline, cancellation і stop conditions залишаються авторитетними межами.

Batch є artifact-oriented, а не SSE transport: `body.stream=true` або
присутній `stream_options` в будь-якому record відхиляють весь Batch під час
create preflight до job ID/cursor/queue admission. Сервер не змінює `stream`
на `false` і не запускає внутрішній stream із прихованим буферизуванням; кожен
допустимий record має один non-streaming Chat Completion у `response.body`.

Batch підтримує тільки `body.n` відсутній або exact `1`. Будь-який інший
`n` відхиляє весь Batch у тому ж bounded create preflight до job ID/cursor/queue
admission: сервер не serializes multiple candidates, не створює кілька output
lines для одного `custom_id` і не приводить значення до `1` неявно.

Batch має окремий fixed greedy sampling profile: `temperature=0`, `top_p=1` і
`top_k=1`. Присутність `temperature`, `top_p`, `top_k`, `seed` або іншого
sampling override хоча б в одному record відхиляє весь Batch у create preflight
до job ID/cursor/queue admission; API не ігнорує або не нормалізує override.
Це deterministic contract лише в межах конкретного runtime build і model
artifact, а не обіцянка byte-identical output між різними Metal пристроями чи
версіями runtime.

Batch підтримує `stop` string або bounded array через той самий validator і
normalizer, що interactive Chat. Malformed або unsupported stop value завершує
лише цей record canonical per-record validation error; valid sequences
передаються в `llm-sampling`, завершують generation з `finish_reason="stop"`
і не повертаються як частина assistant text.

Batch підтримує лише `max_completion_tokens` як completion limit. Присутність
legacy `max_tokens` хоча б в одному record відхиляє весь Batch у create
preflight до job ID/cursor/queue admission: сервер не читає його як alias, не
вибирає неявний пріоритет і не запускає record із застарілим полем.

До Rust-worker tokenizer-aware logit-transform parity `body.logit_bias` не
підтримується: його присутність хоча б в одному record відхиляє весь Batch у
create preflight до job ID/cursor/queue admission. API не ігнорує map і не
передає неперевірені token IDs у Swift worker; майбутня підтримка вимагатиме
family tokenizer validation, bounded bias values і реального застосування в
sampler.

До Rust-worker parity для history-aware logit transforms `presence_penalty` і
`frequency_penalty` не підтримуються: присутність будь-якого з цих полів хоча
б в одному record відхиляє весь Batch у create preflight до
job ID/cursor/queue admission. API не ігнорує penalties; майбутня підтримка
вимагатиме tokenizer-consistent token-history accounting у sampler.

До Rust-worker logprob parity Batch не підтримує `body.logprobs` чи
`body.top_logprobs`: присутність будь-якого з цих полів у record відхиляє весь
Batch у create preflight до job ID/cursor/queue admission. API не ігнорує поля,
не підставляє synthetic значення і не повертає неповні logprobs у
`response.body`; майбутня підтримка вимагатиме real logits-derived contract
worker і conformance fixtures.

До Rust-worker parity для кількох tool calls `parallel_tool_calls` не входить
до Batch surface: його присутність, включно зі значенням `false`, хоча б в
одному record відхиляє весь Batch у create preflight до job ID/cursor/queue
admission. Сервер не serializes цей hint і не вдає його підтримку; майбутнє
ввімкнення вимагатиме explicit capability та conformance fixtures.

`reasoning_effort` не входить до раннього Batch surface: його присутність хоча
б в одному record відхиляє весь Batch у create preflight до job ID/cursor/queue
admission. API не мапить цей control неявно в Qwen-thinking режим і не ігнорує
його для Gemma; підтримка можлива лише після negotiated reasoning capability
Rust worker з family conformance fixtures.

Batch Chat records повністю підтримують `tools` і `tool_choice` тією ж
OpenAI/family validation, що interactive Chat Completions. Tool call повертається
в `response.body` canonical result envelope як звичайний Chat Completion
(`finish_reason="tool_calls"`); він не створює server-side follow-up, не
запускає executable, не передає credentials у worker і не змінює worker queue.
Клієнт виконує call у власній policy boundary та, за потреби, подає наступний
turn як окремий request або інший Batch record.

`tools[].function.strict=true` підтримується лише для schema profile, який
active worker явно оголосив як constrained-decode capability. Невалідна schema
або profile, недоступний для active model/family, завершує лише цей record
typed canonical error; API не знижує strict tool до звичайного tool schema і
не оголошує JSON Schema guarantee без реального constrained decoding.

Batch Chat records підтримують той самий validated `response_format` contract,
що interactive Chat. Спільний validator canonicalizes supported text/JSON mode
та structured constraints перед dispatch; malformed, unavailable для active
model/family або нездійсненний для конкретної генерації constraint завершує
лише цей record canonical per-record error envelope. Сервер не знижує
`json_schema` до best-effort JSON, не змінює `response_format` і не переводить
такий випадок у Batch-wide failure.

`response_format.type="json_object"` гарантує JSON object, а не best-effort
text: generation перевіряється спільним JSON parser перед publication. Якщо
output не є валідним object, завершується лише цей record typed canonical error;
сервер не повертає raw text як успішний JSON-mode completion.

`response_format.type="json_schema"` приймається лише для schema profile, який
active worker явно оголосив constrained-decode capability. Невалідна schema,
profile поза підтримуваним subset або недоступність для active model/family
завершує лише цей record typed canonical error; API не повертає best-effort JSON
і не підміняє `json_schema` на `json_object`.

Strict Batch parity не додає окремої idempotency surface: `POST /v1/batches`
завжди створює новий Batch після успішної валідації. `Idempotency-Key` та інші
недокументовані для цього endpoint headers не впливають на create semantics і
не зберігаються як key-to-Batch mapping. Внутрішня idempotency `job-store`
залишається лише механізмом crash/retry-safe transitions, а не публічним
deduplication contract.

Batch надсилає звичайні generation requests через IPC і ніколи не обходить
межу серіалізації worker. Це OpenAI Batch API, а не GPU micro-batching:
авторитетний worker лишається single-active-generation, доки окремий benchmark
не доведе безпечний інший режим.

Якщо worker crash або IPC transport loss перериває dispatched Batch record без
durable terminal result, dispatcher materializes рівно один `model_error`
envelope для цього record і не надсилає його повторно — незалежно від того,
чи встиг worker надіслати `prepared`. Після worker recovery dispatcher може
продовжити лише наступний недиспатчений record; already terminal result ніколи
не дублюється.

Files, Batch і conversation history зберігаються над IPC через малий
versioned `job-store` contract у спільному global namespace з idempotency і
expiry. У першій версії немає namespace за principal: loopback та всі
автентифіковані remote keys бачать один набір ресурсів.
`job-store-sqlite` є його першою terminal-first durable реалізацією: вона
забезпечує atomic state transitions і restart-safe metadata, але не стає
залежністю worker. Самі upload, output і error JSONL bytes належать
`artifact-store-fs`, а не SQLite BLOB: managed data directory використовує
private staging, hash verification, `fsync` та atomic rename перед публікацією
content-addressed reference. Cleanup видаляє лише artifacts, які SQLite вже
позначив expired/orphaned. Automatic TTL і explicit idempotent delete
визначають retention; expiry прибирає ресурс з API до фактичного GC. Runtime
отримує тільки нормалізований request. Default data root —
`./.turbofieldfare/` відносно current working directory.

Large Batch не розгортається в RAM або worker queue. Input JSONL лишається
artifact file; SQLite зберігає тільки `BatchCursor` (artifact digest,
next-line/byte offset, counters, status і idempotency state). Dispatcher
резервує наступний валідний рядок transactionally та подає його лише коли є
queue slot; output/error JSONL є append-only artifacts, що звіряються з
persisted record state після restart. Так queue залишається bounded незалежно
від розміру Batch.

Кожен published Batch-result line має canonical envelope
`{id,custom_id,response,error}`. `id` — server-generated `batch_req_*`,
`custom_id` точно повторює input, а successful або HTTP-completed request має
`response={status_code,request_id,body}` та `error=null`. Request без
HTTP-відповіді має `response=null` і `error={code,message}`. Успішні 2xx
envelopes йдуть у output JSONL, terminal failures — у error JSONL; у кожному
line немає TurboFieldfare-specific полів.

Успішний Chat Completion `response.body` завжди містить exact
tokenizer-derived `usage.prompt_tokens`, `usage.completion_tokens` і
`usage.total_tokens`; `total_tokens` є їхньою сумою, а не character estimate.
`usage.prompt_tokens_details.cached_tokens` додається лише коли worker точно
виміряв reuse prompt/KV cache; якщо такого вимірювання немає, detail absent, а
не synthetic `0`.

Успішний Chat Completion також містить opaque local `system_fingerprint` від
canonical output-affecting runtime identity: API/worker build, active model
manifest, prompt dialect і config generation. Fingerprint змінюється при
несумісній зміні цих складників та не містить user ID, hardware ID, path,
prompt, credential або інших секретів.

Backend failure у error JSONL використовує bounded public taxonomy
`model_unavailable`, `model_error` або `internal_error` і redacted message.
Published `batch_req_*` `id` є correlation ID для локальних diagnostics; raw
Swift/Metal/OS errors, paths, config values і stack details не потрапляють у
Batch artifact або API error body.

Upload однопрохідно перевіряє JSONL syntax, UTF-8, line boundaries та
мінімальні required fields для Batch record до atomic publish artifact. Це не
виконує family-dependent validation: prompt dialect, context limit і generation
policy перевіряються dispatcher перед reservation конкретного рядка та
потрапляють у його output/error record. Єдиний виняток — Batch-wide exact
`body.model` preflight під час create.

Один Batch input обмежений 50 000 records. Той самий streaming pass, що
перевіряє JSONL, лічить records; 50 001-й record відхиляє весь upload до atomic
publish, не створюючи FileObject, artifact або BatchCursor.

Для підтримуваного Chat Batch кожен input record мусить мати непорожній
`custom_id`, `method: "POST"`, `url: "/v1/chat/completions"` і JSON-object
`body`. `custom_id` унікальний у межах input file; missing/duplicate ID,
інший method/URL або не-object body відхиляють upload до artifact publish.
Server не генерує або не переписує client IDs.

Model/family-dependent semantic failure одного record не зупиняє Batch.
Dispatcher persistently завершує цей record як failed, додає canonical error
envelope до error JSONL, оновлює counters і продовжує cursor з наступним рядком.
Batch переходить у `completed`, коли всі його records terminal, навіть якщо
`request_counts.failed > 0`; клієнт читає partial failure через counters та
error JSONL. Жоден record не пропускається тихо.

`DELETE /v1/files/{id}` робить logical delete input file і в тій самій
transaction позначає всі залежні non-terminal Batch jobs `cancelling`.
Dispatcher доставляє cancellation worker; input bytes утримуються внутрішнім
reference до terminal state кожного job, після чого GC може їх прибрати. Це не
видаляє вже створені output/error artifacts: для них продовжує діяти звичайний
retention policy. Повторний delete та повторна доставка cancellation є
idempotent.

`POST /v1/batches/{id}/cancel` має негайну семантику: transaction переводить
Batch у `cancelling`, знімає його queued records з admission і за потреби
надсилає cancellation active record у worker. Після terminal acknowledgement
Batch стає `cancelled`; кожен record без already durable HTTP result отримує
рівно один canonical error envelope з `response=null` і
`error.code=batch_cancelled`. Уже durable output/error artifacts і counters
зберігаються до свого TTL. Повторний cancel не змінює результат і є
idempotent.
Media не входить у цей етап: майбутній `MediaRef` матиме ID, MIME,
dimensions/bytes limit і explicit architecture capability, а не передаватиме
довільні binary blobs у tokenizer або Metal layer.

### Критерій завершення

- Повний black-box HTTP contract suite проходить із Rust.
- Batch status transitions і canonical result/error JSONL envelopes збігаються.
- Batch приймає лише `completion_window=24h`; expiry припиняє dispatch,
  скасовує queued/active work і переходить у `expired`, не втрачаючи durable
  output/error records.
- Batch `metadata` приймає до 16 string pairs із лімітами key/value 64/512,
  durable зберігається та round-trips без зміни у create/retrieve/list.
- Files upload приймає лише `purpose=batch`; Batch input має `batch`, а
  server-generated output/error artifacts мають `purpose=batch_output`.
- Batch input upload приймає лише filename `.jsonl` до 200 MB; streaming
  overflow або інший filename відхиляється до artifact publish.
- Batch input містить не більш ніж 50 000 JSONL records; 50 001-й record
  відхиляє upload до publish без FileObject, artifact або cursor.
- Files upload повністю підтримує `expires_after` з anchor `created_at` і
  `seconds`: valid policy повертає обчислений `expires_at`, а absent policy —
  default `created_at + 30 days`; malformed або unsupported policy не створює
  artifact.
- Batch create повністю підтримує `output_expires_after` для output/error:
  exact anchor `created_at`, `seconds` 3 600…2 592 000 і `expires_at` кожного
  generated File від його creation time; absent policy зберігає їх до manual
  delete, invalid policy не створює Batch ID.
- `GET /v1/files` не підтримується та не розкриває shared Files namespace;
  retrieve/content/delete конкретного known file ID лишаються підтриманими.
- `GET /v1/batches` реалізує CursorPage `{object:"list",data,first_id,
  last_id,has_more}` з exact `after` та `limit` (1…100, default 20); наступна
  сторінка з `after=last_id` не пропускає та не дублює durable Batch job.
- Batch публічно проходить only valid lifecycle transitions `validating`,
  `in_progress`, `finalizing`, `completed`, `failed`, `expired`, `cancelling`,
  `cancelled`; усі `*_at` timestamps встановлюються один раз на transition і
  однаково round-trip через create/retrieve/list/cancel, включно після restart.
- Batch-level `failed` повертає canonical
  `errors={object:"list",data:[{code,message,param,line}]}`, де absent
  `param`/`line` є `null`; інші statuses мають `errors=null`, а per-record
  failures лишаються тільки в error JSONL.
- Batch create приймає лише `/v1/chat/completions`; інші endpoint-и відхилені
  до job creation, без cursor або worker admission.
- Усі Batch `body.model` мусять exact збігатися з active advertised worker
  model ID; missing або один mismatch відхиляє весь Batch до job ID/cursor/queue
  admission, без неявної model substitution чи per-record fallback.
- `POST /v1/batches` без active ready worker/current model descriptor повертає
  typed `503 model_unavailable` до Batch ID/cursor/queue/durable metadata; API
  не приймає deferred-validation job і не використовує last-known model ID.
- Batch durable pin-ить model ID + manifest/dialect/config generation; mismatch
  active worker перед dispatch дає `model_unavailable` для record без запуску,
  а outputs різних model artifacts не змішуються в одному Batch.
- Batch є text-only: один image, audio, file або інший non-text content part
  відхиляє весь Batch під час streaming create preflight до job ID/cursor/queue
  admission; сервер не strip-ить modality і не створює per-record fallback.
- До cross-family prompt-dialect contract `developer` message в одному record
  відхиляє весь Batch до job ID/cursor/queue admission; сервер не зливає його
  з `system` і не змінює hierarchy інструкцій неявно.
- Batch підтримує canonical `system`/`user`/`assistant`/`tool` history з тією
  ж validation, що interactive Chat: порядок, unique assistant tool-call IDs і
  exact tool `tool_call_id`; malformed history дає canonical per-record error,
  а family adapter не flatten-ить її до user text.
- Кілька `system` messages зберігаються як окремі ordered nodes без merge або
  deduplication; canonical rendered prefix входить у prompt-cache identity.
- Незавершений assistant tool call без усіх відповідних `tool` results дає
  canonical per-record error; server/worker не продовжують generation, не
  виконують tool і не створюють synthetic result.
- Context overflow після canonical render і exact tokenizer count дає
  `context_length_exceeded` для одного record до worker generation; API не
  обрізає messages, system/tool history або prompt, а Batch продовжується.
- За absent `max_completion_tokens` effective completion limit дорівнює всьому
  залишку active context window після prompt render; це все ще обмежено
  context window, Batch deadline, cancellation і stop conditions.
- `body.stream=true` або будь-який `stream_options` відхиляє весь Batch до
  job ID/cursor/queue admission; Batch не coerc-ить stream і не буферизує
  прихований SSE, а повертає лише non-streaming Chat Completion envelopes.
- Batch підтримує лише absent `body.n` або `n=1`; будь-яке інше значення
  відхиляє весь Batch до job ID/cursor/queue admission без multiple candidates,
  кількох output lines на `custom_id` чи неявного clamp до `1`.
- Batch має fixed greedy profile `temperature=0`, `top_p=1`, `top_k=1`;
  `temperature`, `top_p`, `top_k`, `seed` або інший sampling override в одному
  record відхиляє весь Batch до job ID/cursor/queue admission, без silent
  normalization. Determinism обмежений конкретними runtime build і model
  artifact.
- Batch `stop` string або bounded array проходить той самий validator і
  normalizer, що interactive Chat; malformed/unsupported value дає canonical
  per-record error, а valid sequence завершує output із `finish_reason="stop"`
  без повернення sequence у assistant text.
- Batch підтримує лише `max_completion_tokens`; legacy `max_tokens` в одному
  record відхиляє весь Batch до job ID/cursor/queue admission, без alias або
  неявного пріоритету.
- До Rust-worker tokenizer-aware logit-transform parity `logit_bias` в одному
  record відхиляє весь Batch до job ID/cursor/queue admission; сервер не
  ігнорує map і не передає неперевірені token IDs у worker.
- До Rust-worker history-aware logit-transform parity `presence_penalty` або
  `frequency_penalty` в одному record відхиляє весь Batch до
  job ID/cursor/queue admission; сервер не ігнорує penalties.
- До Rust-worker logprob parity будь-який Batch record із `logprobs` або
  `top_logprobs` відхиляє весь Batch до job ID/cursor/queue admission; сервер
  не ігнорує поля та не синтезує або частково не повертає logprobs.
- До Rust-worker parity для кількох tool calls присутній
  `parallel_tool_calls`, включно з `false`, відхиляє весь Batch до
  job ID/cursor/queue admission; сервер не serializes цей hint і не вдає його
  підтримку.
- `reasoning_effort` в одному Batch record відхиляє весь Batch до
  job ID/cursor/queue admission; family-specific mapping можливий лише після
  negotiated reasoning capability Rust worker.
- Batch Chat records підтримують `tools` і `tool_choice` з тією ж валідацією,
  що interactive Chat; `tool_calls` round-trip у canonical `response.body`, а
  сервер/worker ніколи не виконують їх, не мають tool credentials і не
  створюють follow-up turn.
- `tools[].function.strict=true` приймається лише для schema profile, який
  active worker оголосив constrained-decode capability; невалідна або
  unavailable schema дає typed canonical per-record error, без downgrade до
  звичайного tool schema.
- Batch `response_format` використовує той самий validated contract, що
  interactive Chat: malformed, unavailable або нездійсненний structured
  constraint створює один canonical per-record error envelope, без silent
  format downgrade чи Batch-wide failure.
- `response_format.type="json_object"` повертає лише валідний JSON object;
  parser failure дає typed canonical per-record error, без raw-text success.
- `response_format.type="json_schema"` приймається лише для оголошеного
  worker constrained-decode schema profile; невалідна або unavailable schema
  дає typed canonical per-record error, без best-effort JSON чи downgrade до
  `json_object`.
- `body.store=false` не створює local conversation-history record для цього
  Batch record; absent/`true` використовує 7-денну history policy. Усі
  обов'язкові input/output/error artifacts та Batch metadata залишаються
  durable за окремими retention contracts.
- `body.user` в одному record відхиляє весь Batch до job ID/cursor/queue
  admission, доки окремий principal/audit contract не визначить його семантику;
  поле не створює authorization, telemetry identity чи namespace isolation.
- `service_tier` в одному record відхиляє весь Batch до
  job ID/cursor/queue admission: single-worker strict FIFO не має tier,
  priority, credit або SLA semantics і не мапить усі значення в одну чергу.
- До Rust-worker tokenizer-aware prediction-matching parity `prediction` в
  одному record відхиляє весь Batch до job ID/cursor/queue admission; сервер
  не ігнорує поле й не оголошує latency optimization без exact matching.
- Кожен Batch JSONL record має unique non-empty `custom_id`, exact
  `POST /v1/chat/completions` і object `body`; malformed records відхилені до
  artifact publish, без server-generated IDs.
- Два валідні `POST /v1/batches` створюють два різні IDs навіть з однаковим
  `Idempotency-Key`; header не створює TurboFieldfare-specific semantics.
- Large Batch не розгортається в RAM: input/output/error є лише filesystem
  artifacts, а SQLite містить тільки metadata/cursor; restart не дублює рядок
  або output record.
- Invalid JSONL syntax або missing required fields відхиляються до publish;
  model/family-dependent semantic failures оформлюються per-record під час
  dispatch без пошкодження решти Batch.
- Кожен result line є canonical `{id,custom_id,response,error}` без локальних
  полів: HTTP result має `response.status_code/request_id/body` та `error=null`,
  а non-HTTP failure — `response=null` і `error.code/message`.
- Успішний Chat Completion body має exact tokenizer-derived
  `usage.prompt_tokens`, `completion_tokens`, `total_tokens`; cached-token
  detail присутній лише за точного worker measurement, ніколи як estimate.
- Успішний Chat Completion має opaque local `system_fingerprint` від
  output-affecting API/worker/model/prompt/config identity; він не містить
  user/device/path/prompt/secret даних і змінюється за несумісної зміни.
- Backend failure у error JSONL має лише `model_unavailable`, `model_error`
  або `internal_error` і redacted message; `batch_req_*` ID пов'язує локальні
  diagnostics, але raw worker/Metal/OS details у artifact не виходять.
- Semantic failure одного record створює canonical error JSONL envelope й
  durable failed counter; наступні records продовжують виконуватися.
- Batch із terminal records і `request_counts.failed > 0` має статус
  `completed`, а output/error JSONL і counters повністю пояснюють результат.
- Terminal `cancelled` або `expired` Batch materializes кожен record без
  durable HTTP result як рівно один error JSONL envelope з відповідно
  `batch_cancelled` або `batch_expired`; сума completed і failed дорівнює total.
- Restart API не втрачає Files/Batch metadata, global lifecycle, expiry або
  idempotent job result; це перевіряється проти SQLite backend.
- Interrupted upload або output publish не робить частковий artifact видимим;
  restart/GC safely прибирає staging і підтверджені orphaned files.
- Input `purpose=batch` зникає з API через default 30 днів або свій
  валідний `expires_after` до physical GC;
  server-generated `purpose=batch_output` не має automatic TTL і видаляється
  лише вручну. Manual delete та повторний delete є безпечними й idempotent.
- Delete input file atomically переводить усі залежні non-terminal Batch jobs
  у `cancelling`, доставляє worker cancellation і не прибирає їхні input bytes
  до terminal states; already-produced output/error лишаються доступними.
- Batch cancel не dispatch-ить нових records, знімає queued records, передає
  cancellation active record і стає `cancelled` після terminal acknowledgement;
  existing output/error/counters зберігаються, а кожен не завершений record
  має canonical `batch_cancelled` error envelope.
- Cancellation правильно доходить до queued та active work.
- Семантика server restart явно визначена й протестована.
- Job transitions і output/error JSONL є idempotent за request/job IDs після
  API restart; worker crash materializes один `model_error` для interrupted
  record без automatic re-generation, а completed output не дублюється.
- SwiftNIO більше не потрібен у production launch path.

## Етап 5: GTurbo V1, MoE descriptors і family adapters без Metal

Роботу розділити між `gturbo-format`, `gturbo-store`, `moe-core`, `llm-prompt`,
`gemma4-arch` і `qwen36-arch`. Format crate описує on-disk bytes і layout,
store реалізує bounded reads та integrity, MoE core — traits і topology, а
кожен family adapter — semantic schema, tensor catalog, prompt dialect та layer
graph.

Перенести без Metal execution:

- parsing manifest і verified-install receipt
- common structural validation format та role-based quantization descriptors
- resident index і packed-expert layout
- file sizes, hashes, offsets, strides і alignment
- Gemma 4 і Qwen 3.6 architecture adapters та явний registry
- load, encode, decode та streaming decode tokenizer для обох families
- Gemma IT і Qwen ChatML templates та structured tool-call parsers
- Qwen hybrid full-attention/Gated-DeltaNet descriptor і optional MTP metadata

Не змінювати `.gturbo`. Одночасна зміна model format забере можливість
незалежно порівнювати host-реалізації.

### Критерій завершення

- Rust codecs читають accepted Gemma artifacts і fixtures із PR №105 та
  побайтово стабільно round-trip власні GTurbo metadata.
- Token IDs, rendered prompts і tool-call structures збігаються з golden
  fixtures окремо для Gemma та Qwen.
- Додавання synthetic третьої architecture реалізується test-only adapter без
  змін у `moe-core`, `gturbo-store` або `moe-runtime`.
- Corrupt, truncated, misaligned або incompatible models безпечно відхиляються.
- Жоден test або loader не розміщує tensor масштабу моделі в Rust heap.
- `gturbo-format` має standalone inspect example без Metal і inference.
- Family prompt/manifest adapters тестуються без model weights і GPU.

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
- `metal-runtime` не імпортує family-specific constants або pipelines.

## Етап 7: спільні MoE kernels та family-specific pipelines

У `moe-metal-kernels` винести лише справді спільні primitives. У
`gemma4-metal` і `qwen36-metal` повторно використати наявний MSL і замінити
лише Swift host orchestration. Не узагальнювати kernels тільки через однакове
ім'я операції, якщо layout, gating або numerical contract відрізняються.
Рекомендований порядок:

1. embedding lookup
2. RMSNorm
3. RoPE
4. affine GEMV для реально підтриманих 4/6/8-bit layouts
5. sampling, stop і family-specific logit transforms
6. full/sliding attention
7. shared expert і routed MoE
8. family-specific fused QKV/layer-tail/head paths
9. Qwen attention output gate і Gated-DeltaNet
10. optional Qwen MTP verification path після базового Qwen parity

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

Критерій архітектурної зрілості цього етапу: Gemma і Qwen використовують одну
Metal runtime/command abstraction та спільні primitives там, де числовий
контракт справді однаковий, але жоден family adapter не перевіряє назви tensors
іншої family.

## Етап 8: пам'ять моделі та expert streaming

Розмістити загальний bounded storage у `gturbo-store`, а slot planning і
MoE-специфічний lifecycle — у `moe-expert-cache`.

Перенести:

- read-only mapping спільних weights
- no-copy Metal buffer wrapping
- aligned expert-slot allocation
- positional `pread`
- lazy layer opening і verification
- bounded LFU cache із налаштовуваною кількістю slots і recency tie-break
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
- Ті самі cache/storage crates проходять fixtures для Gemma 128 experts і
  Qwen 256 experts без hard-coded counts або layer filenames.

## Етап 9: Rust decode worker

`moe-runtime` вибирає `gemma4-arch` або `qwen36-arch` за перевіреним manifest і
адаптує executor до `inference-core`; `worker` додає IPC, авторитетну чергу і
lifecycle. Жоден із цих шарів не дублює OpenAI validation.

Перенести:

- model ownership і runtime configuration
- KV/state stores для sliding-window, full-attention і Qwen linear attention
- family-owned layer graph замість hard-coded циклу з 30 layers
- загальне планування `router -> top-k -> expert I/O -> expert execution`
- overlap shared-expert/read
- tied або untied head, sampling, stops і family detokenization
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
- Gemma і Qwen запускаються тим самим worker binary через registry selection;
  unsupported architecture відхиляється до allocation model-scale buffers.

## Етап 10: Rust prefill і Apple tensor paths

Спочатку перенести production behavior без нових політик. Chunk sizes,
projection paths і layer mix беруться з architecture adapter та capability
policy, а не зі спільних hard-coded Gemma defaults:

- прийняті production chunks окремо для Gemma і Qwen
- вибір GEMV/QMM залежно від projection і shape
- обмежений reusable scratch
- staged affine 4/6/8-bit paths для заявлених adapter capabilities
- batched routed MoE
- language-model head лише для фінального row
- Apple10 TensorOps full-attention path
- Qwen Gated-DeltaNet prefill/state path
- causal-tiled fallback для попередніх GPU families

Вибір має залежати від capabilities, а не від назви chip. TensorOps є бажаним
portable MSL primitive там, де виграє його shape, і використовує neural
accelerators M5 для dense compute на кшталт LLM prefill. Single-token decode
залишається bandwidth-oriented і зберігає custom packed GEMV paths, доки
вимірювання не доведуть перевагу іншого підходу.

### Критерій завершення

- Проходять прийняті short/medium/long prefill gates для обох families.
- Full-attention gates покривають 8K, 16K, 32K і 64K.
- Qwen hybrid-layer і linear-state gates проходять окремо для 4/6/8-bit.
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
bounded range transport, `repack-core` — за checkpoint/writer policy,
`gemma4-repack` і `qwen36-repack` — за source tensor mapping та quantization
plan, а `gturbo-format` — за target layout. Зберегти:

- curated `ModelSourceDescriptor`: family, source format, pinned repo/revision,
  expected index/digest, allowed redirect hosts і credential policy
- installer не має `--repo-id`, `--revision` або custom-source mode у фазах
  міграції; QAT і довільні checkpoints — окремий post-parity product decision
- bounded HTTP range downloads
- відсутність повного checkpoint або shard на disk
- tile-sized scratch
- прямий запис у фінальний resident/expert layout
- durable range checkpoints
- cancellation і resume
- explicit partial discard
- hashes, receipt binding, advisory locking і atomic promotion

Safetensors читається тільки через `model-source`/family repack adapter.
`hf-hub` може надати Hub metadata та authentication, але реалізація має
зберегти bounded range transport, а не непомітно завантажувати повні source
shards. Authorization може пережити redirect лише до explicit allowlist CDN
hosts; на будь-якому іншому host він знімається. URLs, tokens та headers не
потрапляють у telemetry, errors або receipt.

Custom source/QAT підтримка не є просто ще одним CLI flag: вона потребує
окремого trust policy, user-visible storage/throughput estimate і незалежних
repack/load/generation fixtures. Повернутися до неї можна після Rust installer
parity, але вона не блокує Gemma/Qwen curated migration.

### Критерій завершення

- Rust output відповідає GTurbo V1 codecs і accepted Swift output окремо для
  Gemma та Qwen 4/6/8-bit fixtures.
- Перервані installs продовжуються після перевірки завершених ranges.
- Пошкоджені ranges завантажуються повторно.
- Peak scratch залишається обмеженим.
- Повний source checkpoint не створюється.
- Receipt прив'язаний до curated descriptor; redirect, 403 та resume cases
  проходять без витоку bearer token.

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
turbofieldfare install --family qwen36 --bits 6 --output scratch/qwen36.gturbo
turbofieldfare serve --model scratch/qwen36.gturbo --port 8080
turbofieldfare inspect --model scratch/qwen36.gturbo
turbofieldfare benchmark --model scratch/qwen36.gturbo
```

`serve` може керувати окремими API і worker processes, залишаючись однією
командою для оператора.

Усі три binaries залишаються тонкими: CLI лише керує командами/processes, API
binary компонує HTTP з IPC client, а worker binary компонує IPC service з
runtime. Інші проєкти можуть зібрати власний server, worker або embedded runtime
із тих самих library crates.

### Окремий security milestone: general `--host`

Після terminal/API parity додати general bind як окремий продуктний етап, не як
зміну default у `serve`:

- `--host 127.0.0.1` лишається default і працює без credentials;
- `--host <non-loopback>` вимагає explicit remote-security config; відсутня,
  неповна або невалідна конфігурація завершує запуск до bind;
- remote mode має TLS server certificate/key і static Bearer API keys зі
  hashed key file, bounded request/connection admission, rate limits, audit
  events без prompt/secret content і documented credential rotation;
- Bearer authentication у першій remote версії захищає доступ до одного
  спільного Files/Batch/history namespace, а не створює ізоляцію між keys;
  per-principal authorization є окремим майбутнім security/product decision;
- TLS і API keys не потрібні та не застосовуються до loopback mode;
- `/health` має окрему, навмисно визначену policy; інші OpenAI routes не
  допускають anonymous access;
- security tests покривають wildcard/LAN/IPv6 binds, missing/expired TLS,
  invalid credentials, queue exhaustion, log redaction та loopback regression.

Bearer API key є першим remote identity mechanism; mTLS може бути доданий
пізніше як другий provider. Tool runner не є наслідком remote bind і не входить
у scope цього продукту: API/worker ніколи не отримують tool credentials або
права виконання.

## Наскрізні приймальні критерії

Компонент не видаляється, доки заміна не пройде всі відповідні перевірки:

| Вимір | Критерій |
| --- | --- |
| HTTP | Повний паритет black-box contract |
| Коректність | Проходять kernel/reference і generation fixtures обох families |
| Якість | Gates delta-NLL і top-k проходять для кожної family/quantization |
| Decode | Не менше 98% відповідного Swift family baseline |
| Prefill | Не менше 95% відповідного fallback; без регресії M5 TensorOps |
| TTFT | Регресія не більше 5% |
| Пам'ять | Регресія physical footprint не більше 10% |
| I/O | Обмежені reads, slots, descriptors і queues |
| Cancellation | Перевірено lifecycle HTTP -> API -> worker -> Metal |
| Безпека | Немає другого model process; non-loopback bind fail-closed без TLS і Bearer key; loopback не вимагає TLS |
| Tools | Server/worker не виконують tools і не зберігають tool credentials; клієнт володіє execution loop |
| Streaming | Monotonic event sequence, lossless UTF-8 і рівно один terminal event |
| Надійність | Worker readiness/crash/reconnect не дають stale socket, orphan або duplicate output |
| Cache | Cache key містить template/dialect/config identity; кожен miss має reason code |
| Observability | Ранній envelope показує phase/cache; Rust worker додає I/O/GPU/memory; disabled path виміряний |
| Source trust | Receipt містить descriptor provenance; redirects і credentials проходять allowlist/redaction gates |
| Архітектура | Немає циклів; executable crates не містять доменної логіки |
| Повторне використання | Публічні crates мають examples і збираються поза product binary |
| Пакування | `cargo package` проходить для кожного кандидата на публікацію |
| Сумісність | Wire protocol, public Rust API і `.gturbo` versioned незалежно |
| Розширюваність | Synthetic третя MoE family додається без змін у shared core |

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
-> GTurbo V1 codecs + Gemma/Qwen descriptors на Rust
-> спільний MoE Metal primitive із Rust host
-> Gemma first token -> Gemma decode/prefill parity
-> Qwen first token -> Qwen decode/prefill parity
-> Qwen 4/6/8-bit і optional MTP parity
-> production worker на Rust
-> Gemma/Qwen installer adapters на Rust
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
6. додати `event_seq`/terminal-event invariants у mock contract;
7. визначити redacted telemetry envelope та cache miss reason codes, не
   додаючи ще реального IPC чи Metal instrumentation.

Цей крок не змінює inference runtime, IPC integration чи наявні Swift targets.
`RuntimeConfig`, profile/default semantics, architecture capabilities і
constraint negotiation відкладені до Rust worker. Після цього етапу Phase 2
може додати мінімальний transport, не ламаючи публічний API server або
model-specific crates.
