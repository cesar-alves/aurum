# Configuration

## Precedence

1. **CLI flags**
2. **Environment**: provider secrets (`OPENROUTER_API_KEY`, `OPENAI_API_KEY`, `ELEVENLABS_API_KEY`, `XAI_API_KEY`), OpenRouter base URL, and TTS (`AURUM_TTS_*`)
3. **Config file**
4. **Built-in defaults**

A provider is **never** selected merely because its API key is present. Omitted STT/TTS `provider` remains `local`.

## Environment

| Variable | Purpose |
|----------|---------|
| `OPENROUTER_API_KEY` | OpenRouter auth (preferred over file `api_key`) |
| `OPENROUTER_BASE_URL` | Override OpenRouter API base (tests / proxies) |
| `OPENAI_API_KEY` | OpenAI provider-scoped secret |
| `ELEVENLABS_API_KEY` | ElevenLabs provider-scoped secret |
| `XAI_API_KEY` | xAI provider-scoped secret |
| `AURUM_TTS_MODEL` | Override `[tts].model` |
| `AURUM_TTS_VOICE` | Override `[tts].voice` |
| `AURUM_TTS_LANGUAGE` | Override `[tts].language` |
| `RUST_LOG` | Tracing filters |

Secrets are stored as redacting `SecretString` values. Effective-config / doctor / support output shows **presence only** (`***`), never plaintext.

## Config file

Resolved via the `directories` crate (app name `aurum`):

| Platform | Typical path |
|----------|----------------|
| macOS | `~/Library/Application Support/aurum/config.toml` |
| Linux | `~/.config/aurum/config.toml` |
| Windows | `%APPDATA%\aurum\config.toml` |

### Canonical schema

```toml
[stt]
provider = "local"          # local | openrouter | openai | xai
model = "base"
language = "auto"
# output = "txt"            # txt | srt | json

[tts]
provider = "local"          # local | openrouter | openai | elevenlabs | xai
model = "kitten-nano-int8"
voice = "Luna"
language = "en"
speaking_rate = 1.0
max_chars = 5000
timeout_ms = 120000
# Optional local pack override (directory with aurum-tts-manifest.json — not a bare .onnx)
# pack_dir = "/path/to/pack"
# allow_unverified = false

# Optional custom catalogue entries (JOE-1620). Never shadow built-in ids.
# [[tts.custom_models]]
# id = "my-tone"
# adapter = "fake-sine-v1"
# pack_dir = "/path/to/pack"
# trust = "verified"   # verified | local_unverified (never builtin)
# license = "CC0"

[cleanup]
style = "raw"              # raw | clean | bullets | professional | summary
provider = "rules"         # rules | openrouter
# openrouter_model = "google/gemini-2.5-flash-lite"

# Named provider options + optional file secrets (prefer env vars for keys).
# Unknown [providers.*] keys fail closed.

# [providers.openrouter]
# stt_mode = "auto"          # auto | chat | transcriptions
# model = "google/gemini-2.5-flash"
# base_url = "https://openrouter.ai/api/v1"
# allow_custom_endpoint = false
# use_system_proxy = false

# [providers.openai]
# base_url = "https://api.openai.com/v1"

# [providers.elevenlabs]
# [providers.xai]

# An optional reviewed deployment catalogue. Its records replace matching
# built-ins by canonical id; `enabled = false` may name the canonical id or an
# existing alias and removes that entire record. `[defaults.stt].global` is
# resolved after the effective catalogue is assembled, so it may target a
# built-in; it must be a `supported` record (never experimental). Every record
# is local (`provider = "local"`): remote models are chosen with `--provider`
# plus credentials and are never catalogue records.
# The path is explicit: Aurum never discovers, fetches, or falls back from it.
# A relative path resolves against this config file's directory (not the cwd).
# [catalogue]
# path = "/absolute/path/to/model-catalogue.toml"
```

Only canonical sections are accepted: `[stt]`, `[cleanup]`, `[tts]`, `[providers.*]`, `[catalogue]`.
Unknown top-level sections (including old `[default]` / `[openrouter]`) fail closed.

A catalogue can only set a single global default per direction, and that
default must be a local `supported` record. Experimental and explicit-only
records (such as the Portuguese specialists) are reachable through an explicit
`[stt].model` / `--model` only; `language` never selects a model. The
diagnostic catalogue digest identifies the effective records, not the
deployment file location, so relocating an unchanged catalogue does not
invalidate a resumable batch.

#### Deployment catalogue trust model

`[catalogue].path` is a **trusted, operator-owned input**. Treat it like
shipping model weights, because its records decide which hosts Aurum downloads
weights from:

- **Allowlist semantics.** Every `url` in a deployment catalogue is an
  operator-approved download origin. Aurum requires HTTPS without embedded
  credentials, plus an exact size and SHA-256 pin for every artifact, but it
  does not restrict the host. Only point the path at files that you review and
  control.
- **Path resolution.** A relative path resolves against the directory of the
  config file that names it. It never resolves against the process working
  directory, and it is an error when no config file is in use. An absolute
  path is recommended.
- **File policy.** The target must be a regular file of at most 1 MiB. A
  symlink is refused, not followed. Aurum reads the file once at startup and
  fails closed on any read, size, schema, or validation error. There is no
  environment-variable, discovery, or built-in fallback.

The embedded catalogue is held to a stricter rule. Its URLs may only use the
reviewed hosts (`huggingface.co`, plus GitHub release assets), and every
Hugging Face URL must name an immutable revision.

### `local_only`

When `local_only` is set on the runtime/validated config (CLI offline flag and library builders), validation rejects a remote STT or TTS provider **before** encoding, upload, or request construction.

## Safety limits

| Limit | Default |
|-------|---------|
| Max duration (STT) | ~2.25 hours |
| Max decoded PCM (STT) | ~500 MB (enforced during decode) |
| Max remote upload (STT) | ~24 MB compressed |
| Max TTS characters | 5000 (`[tts].max_chars`) |
| TTS timeout | 120000 ms (`[tts].timeout_ms`) |
| TTS speaking rate | 1.0, allowed range `0.5..=2.0` (CLI clamps the same) |

Whisper special tokens such as `[BLANK_AUDIO]` are stripped. Segment timestamps
are clamped to audio duration.
