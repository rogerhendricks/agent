# BMO Agent (Rust)

BMO is a local-first desktop voice and text assistant written in Rust. It combines:
- Whisper STT for local transcription
- Ollama for local LLM reasoning and tool calling
- Piper TTS for speech output
- Egui for a live desktop interface with activity logs

## Implemented Features

- Skill registry based on a `Skill` trait and dynamic JSON dispatch.
- Built-in skills:
    - `get_time`
    - `web_search` (SearXNG JSON API)
    - `n8n_task` (custom webhook)
    - `get_weather` (wttr.in)
    - `play_music` (`playerctl` integration)
    - `run_sub_task` (bounded sub-agent loop)
- Voice pipeline:
    - microphone capture and silence detection
    - Whisper transcription
    - tool loop and conversational response
    - Piper speech playback
- GUI capabilities:
    - animated BMO visualizer
    - live settings for Ollama model/URL, SearXNG URL, and n8n URL
    - scrollable logs for user, assistant, tool calls/results, and errors
    - manual text message input

## Runtime Requirements

### Rust

- Rust toolchain (stable)

### Linux audio and native build dependencies

Install the package set for your distro:

Fedora:
```bash
sudo dnf install -y gcc gcc-c++ make cmake alsa-lib-devel
```

Arch:
```bash
sudo pacman -S --needed base-devel alsa-lib
```

Ubuntu/Debian:
```bash
sudo apt-get update
sudo apt-get install -y build-essential cmake libasound2-dev
```

### External binaries

- `ollama` server must be reachable
- `piper-tts` must be in `PATH`
- `aplay` must be in `PATH`
- `playerctl` is required for the music skill

### Model files expected in project root

- `ggml-tiny.en.bin`
- `bmo.onnx`
- `bmo.onnx.json`

The app now performs startup preflight checks and logs missing requirements in the UI.

## Build and Run

```bash
cargo check
cargo run --release
```

## First-Time Configuration in UI

Open the settings panel and set:
1. Ollama URL (default: `http://127.0.0.1:11434/api/chat`)
2. Model name (default: `llama3`)
3. SearXNG URL
4. n8n webhook URL (optional unless using `n8n_task`)

## Quick Validation Flow

Run these prompts in order:
1. `What time is it?`
2. `Search for latest Rust release news`
3. `What is the weather in Berlin?`
4. `Pause music`
5. `Trigger n8n workflow for test payload`

If something is misconfigured, errors should appear in the on-screen logs.

## Extending Tools

To add a new capability:
1. Implement the `Skill` trait.
2. Register it in `SkillRegistry::new`.
3. Provide a clear `description()` so the model knows how to call it.

No enum router is required in the current architecture.
