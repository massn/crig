# crig (construct-rig)

[![crig logo](docs/logo.svg)](#crig-construct-rig)

CLI tool for managing LLM and Agent configurations. Jack into your custom constructs.

## Features

- Jack in: Launch Claude Code with custom construct configurations
- Easy interactive construct configuration for LLM and Agent settings
- Graphical TUI to visualize and select constructs
- Written in Rust for high performance

## What is a Construct?

A **construct** is a configured virtual environment (like a profile or preset). In cyberpunk terminology, "the construct" refers to a simulated reality or virtual space. In crig, each construct contains:

- Agent type (e.g., Claude Code)
- LLM configuration (API or local)
- Model settings
- Interface path (Claude Code executable)

Think of it as a complete virtual environment configuration that you "jack into" to work with Claude Code under specific settings.

## Installation

### From Source

```bash
git clone https://github.com/massn/crig.git
cd crig
cargo build --release
```

## Usage

### Create or Update Configuration

Configure constructs interactively:

```bash
crig config
```

You can configure:
- Construct name (cannot be "default" — that is reserved as an alias for the active construct)
- Agent type (Claude Code / Custom)
- LLM type (Claude API / Anthropic-compatible / Ollama)
- Detailed settings like API keys and endpoints

### List and Jack In via TUI

View all constructs and jack into one interactively:

```bash
crig list
```

![crig show](docs/list.png)

The TUI consists of three sections:

- **Constructs** — lists all configured constructs. The selected one is highlighted with `▶`. Use `k`/`j` or arrow keys to navigate.
- **Selected Construct Flow** — visualizes the selected construct as a flow: Local PC (Agent) `>>` connection `>>` LLM. Shows agent type, model name, and API key status.
- **Bottom bar** — keyboard shortcuts.

Press `Enter` to jack into the selected construct. Press `q` or `Esc` to quit.

### Jack In

Jack into a construct directly:

```bash
# Jack in with the active construct
crig jack
crig jack default

# Jack in with a specific construct and transmit neural payload
crig jack my-construct --help
crig jack my-construct "write a hello world program"
```

The `jack` command will:
- Initialize neural connection to the specified construct
- Establish interface through the configured Claude Code path
- Transmit any payload arguments to Claude Code with construct's LLM settings

### Custom Config Path

Use a custom config file location:

```bash
crig --config-path ~/my-configs/crig.toml config
crig --config-path ~/my-configs/crig.toml list
```

## Configuration File

Configuration is saved at:

- Default: `~/.config/crig/config.toml`

You can specify a custom config path using the `--config-path` option.

### Configuration File Format

Example `config.toml`:

```toml
active_construct = "remote"

[[constructs]]
name = "remote"
agent_type = "claude_code"
claude_code_path = "claude"

[constructs.llm_config]
type = "claude_api"
model = "claude-sonnet-4-6"

[[constructs]]
name = "local"
agent_type = "custom"
claude_code_path = "/usr/local/bin/claude"

[constructs.llm_config]
type = "anthropic_compatible"
endpoint = "http://localhost:8080"
model = "llama3"

[[constructs]]
name = "ollama"
agent_type = "claude_code"
agent_path = "claude"

[constructs.llm_config]
type = "ollama"
endpoint = "http://localhost:11434"
model = "llama3"
# Optional. Sent as ANTHROPIC_AUTH_TOKEN to the agent. Defaults to "ollama".
# Useful when pointing at a remote Ollama-compatible endpoint that requires auth.
# api_key = "sk-..."
```

## Examples

### Configuration with Claude API

```bash
$ crig config
=== crig Configuration ===

Construct name: remote
Select agent type: Claude Code
Select LLM type: Claude API
Claude API key (leave empty to use environment variable):
Model name: claude-sonnet-4-6
Claude Code path: claude

✓ Construct 'remote' saved successfully!
```

### Configuration with an Anthropic-compatible endpoint

```bash
$ crig config
=== crig Configuration ===

Construct name: local-llm
Select agent type: Custom
Select LLM type: Anthropic-compatible
Endpoint URL: http://localhost:8080
Model name: llama3
Claude Code path: /usr/local/bin/claude

✓ Construct 'local-llm' saved successfully!
```

### Jacking In

```bash
$ crig jack remote
>> Initializing neural connection...
>> Construct: remote
>> Interface path: claude
>> LLM: claude-sonnet-4-6 (Cloud)

>> Jacking in...

# Claude Code starts with the configured construct settings
```

## Supported Agents

- Claude Code
- Custom (for future extensions)

## Supported LLMs

- Claude API (Anthropic)
- Anthropic-compatible endpoint (any local or remote `ANTHROPIC_BASE_URL`-compatible server)
- Ollama (with lifecycle management — crig can start/stop `ollama serve` for you)

### Ollama control

crig can manage a local Ollama server. Pick `Ollama` as the LLM type in `crig config`, or drive it directly:

```bash
crig ollama start    # start `ollama serve` detached (no-op if already running)
crig ollama status   # show whether ollama is reachable
crig ollama stop     # stop the server started by `crig ollama start`
```

All three accept `--endpoint <url>` (default `http://localhost:11434`).

When you `crig jack` into an `Ollama` construct, crig will:

1. Check whether the endpoint host is local (loopback) or remote.
2. **Local endpoint** (e.g. `http://localhost:11434`):
   - If ollama is already reachable, use it as-is and leave it running after you exit.
   - Otherwise spawn `ollama serve` for the session and stop it when you jack out.
   - Pull the requested model on demand via `ollama pull` if it is not present.
3. **Remote endpoint** (e.g. `https://llm.example.com/`):
   - Verify reachability (TCP probe; default port 443 for `https://`, 11434 for `http://`).
   - Do **not** start a local `ollama serve` or attempt `ollama pull`.

### Auth token for Ollama-compatible endpoints

The Ollama LLM type accepts an optional API token. crig passes it to the agent
as the `ANTHROPIC_AUTH_TOKEN` environment variable (defaulting to the literal
string `"ollama"` when unset, which is what a vanilla local Ollama expects).

You can set it interactively via `crig config`, or directly in `config.toml`:

```toml
[constructs.llm_config]
type = "ollama"
endpoint = "https://llm.example.com/"
model = "gemma3:27b"
api_key = "sk-..."
```

crig configures the agent process by **setting environment variables directly on
the spawned child** (`ANTHROPIC_BASE_URL`, `ANTHROPIC_AUTH_TOKEN`,
`ANTHROPIC_API_KEY`, `CLAUDE_CODE_USE_BEDROCK`). No `settings.json` is written
to disk, so the token never lands on the filesystem and your persistent
`~/.claude/settings.json` is left untouched.

## Development

```bash
# Build
cargo build

# Run in development mode
cargo run -- config

# Release build
cargo build --release
```

### Secret scanning (required setup)

This repo uses [gitleaks](https://github.com/gitleaks/gitleaks) to prevent accidental commits of API keys, tokens, and other secrets. CI runs gitleaks on every push and PR; locally, enable the pre-commit hook once per clone:

```bash
# Install gitleaks
brew install gitleaks   # or see https://github.com/gitleaks/gitleaks#installing

# Point git at the versioned hooks directory
git config core.hooksPath .githooks
```

If the hook flags a false positive, add an entry to `.gitleaks.toml` or annotate the line with `# gitleaks:allow`.

## License

MIT

## Contributing

Issues and Pull Requests are welcome!
