# Walkthrough: Current Runtime Path

This walkthrough matches the current implementation and is intended for real verification, not historical summary.

## 1. Start the App

From project root:

```bash
cargo check
cargo run --release
```

On launch, read the in-app logs first. Startup checks now report:
- required model files
- required binaries (`piper-tts`, `aplay`, `playerctl`)
- microphone availability
- Ollama reachability via health endpoint

## 2. Configure Settings

Open the settings section and verify:
1. Ollama URL points to your chat endpoint.
2. Model name exists in your Ollama instance.
3. SearXNG URL points to a reachable server.
4. n8n webhook URL is set if using workflow automation.

## 3. Validate Text Flow First

Use the text box for deterministic checks:
1. `What time is it?`
2. `Search for rust async runtime updates`
3. `What is the weather in Sydney?`
4. `Pause music`

Expected behavior:
- each request appears as a user log entry
- tool call and tool result logs are shown when a skill is used
- assistant reply appears after tool observation round-trip
- TTS is attempted for final assistant response

## 4. Validate Optional Integrations

For n8n:
1. Set webhook URL.
2. Send a prompt such as `Run n8n task to test webhook with payload`.
3. Confirm status and response text in logs.

For sub-agent:
1. Send a delegation prompt such as `Research two recent Rust web framework updates and summarize`.
2. Confirm bounded sub-agent step logs and final merged summary.

## 5. Validate Voice Flow

After text is stable:
1. Speak a short command.
2. Pause for silence detection.
3. Confirm state transitions: listening -> thinking -> speaking -> idle.

If voice output fails, errors from Piper/aplay are now logged in the UI.

## 6. Troubleshooting Signals

Use startup and runtime logs as source of truth:
- missing file errors indicate model placement issues
- missing command errors indicate PATH/dependency issues
- Ollama warnings indicate endpoint/network mismatch
- skill failures usually include actionable cause in tool result/error entries
