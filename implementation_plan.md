# Implementation Plan: Stabilization and Runtime Hardening

This document reflects the current state of the project and defines the remaining work to make the app reliably runnable end-to-end.

## Current State Summary

Implemented in `src/main.rs`:
- Trait-based skill registry with dynamic JSON tool dispatch.
- Skills: `get_time`, `web_search` (SearXNG), `n8n_task`, `get_weather`, `play_music`, `run_sub_task`.
- Egui desktop UI with animated face visualizer, settings panel, text input, and scrollable activity log.
- Voice pipeline: microphone capture -> Whisper STT -> Ollama reasoning -> tool execution -> Piper TTS.

Recently added hardening:
- Startup preflight checks for required files and runtime commands.
- Startup microphone detection.
- Startup Ollama reachability check.
- GUI log visibility for AI pipeline crashes and TTS playback failures.

## Remaining Work

### Phase 1: Runtime Validation
1. Run `cargo run --release` with all local dependencies available.
2. Confirm startup checks appear in the UI log and correctly report failures.
3. Verify no immediate panic if optional integrations are unavailable.

### Phase 2: End-to-End Behavior Tests
1. Test text flow first:
   - Ask for time.
   - Trigger web search.
   - Trigger weather lookup.
2. Test command-dependent skills:
   - Trigger `play_music` with `play`/`pause`.
   - Trigger `n8n_task` with configured webhook.
3. Test voice flow once text is stable.

### Phase 3: Scope Decisions
1. Decide whether `run_sub_task` remains enabled by default.
2. Decide whether to keep `play_music` if `playerctl` is missing on target machines.
3. Defer persistence and packaging until baseline runtime is consistently stable.

### Phase 4: Documentation Maintenance
1. Keep README and walkthrough synchronized with the code and startup checks.
2. Keep this plan focused on remaining work only.

## Verification Checklist

Automated:
1. `cargo check`
2. `cargo run --release`

Manual:
1. Type: "What time is it?" and confirm tool call, tool result, final assistant response, and TTS attempt.
2. Type: "Search for Rust news" and confirm SearXNG results are returned.
3. Type: "What is the weather in Tokyo?" and confirm weather output.
4. Type: "Pause music" and confirm `playerctl` path works or reports a clear error.
5. Configure n8n URL and trigger an automation request.
6. Use a short voice prompt and confirm STT -> LLM -> TTS cycle.
