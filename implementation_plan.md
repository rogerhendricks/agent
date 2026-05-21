# Implementation Plan: Extensible Console Agent with SearXNG and Premium GUI

This plan details the implementation of a highly extensible voice agent in Rust. It defines a dynamic skill registry, integrates your self-hosted **SearXNG** search instance, supports **n8n webhooks**, resolves existing compile issues, and upgrades the Egui GUI to a state-of-the-art interface.

## User Review Required

> [!IMPORTANT]
> **ALSA Development Library Dependency (Fedora Linux)**
> During check, compilation failed because the system library `alsa` is missing. 
> To resolve this, you need to install the ALSA development package on Fedora:
> ```bash
> sudo dnf install -y alsa-lib-devel
> ```
> Please confirm if you would like me to run this installation for you or if you prefer to run it yourself.

> [!NOTE]
> **SearXNG Integration**
> We will query SearXNG using its native JSON API: `https://<searxng-url>/search?q=<query>&format=json`.
> We will add a **SearXNG URL** input field to the GUI settings panel so you can easily configure it (e.g. `http://localhost:8080` or your custom domain).

## Proposed Changes

We will restructure `src/main.rs` to clean up syntax issues, implement the extensible skill system, support SearXNG and n8n, and upgrade the GUI.

### Extensible Skill Architecture

We'll define a `Skill` trait and a thread-safe registry:

```rust
pub trait Skill: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn execute(&self, args: &serde_json::Value, config: &AppConfig) -> Result<String>;
}
```

We will implement three default skills:
1. **GetTimeSkill**: Returns local system time.
2. **WebSearchSkill**: Queries your custom **SearXNG** instance via JSON and formats the top 3-4 results for the LLM.
3. **N8nTaskSkill**: Sends custom task payloads to your self-hosted **n8n** webhook URL.

---

### File Changes

#### [MODIFY] [main.rs](file:///home/roger/Projects/agent/src/main.rs)

We will update `src/main.rs` to do the following:
1. **Fix Compile Errors**: Remove syntax noise (e.g. the stray `Agent` / `agent` around line 30).
2. **Implement Skill Registry**:
   - `Skill` trait and registry.
   - Dynamic prompt generation and dynamic dispatch for actions.
3. **Enhance AppConfig**:
   - Add `searxng_url` and `n8n_url` to `AppConfig` and register the skills.
4. **Upgrade GUI (Egui)**:
   - Integrate a premium dark-themed layout.
   - Add settings input fields for Ollama URL, Model, SearXNG URL, and n8n webhook URL.
   - Include a scrollable, colorful conversation log showing user transcripts, active tool execution statuses, and AI replies.
   - Provide a manual message text box in case the user wants to type commands instead of using the mic.

---

## Verification Plan

### Automated Verification
Once ALSA development headers are installed, we will run:
- `cargo check` to verify types and dependencies.
- `cargo build` to ensure the binary compiles successfully.
- `cargo run` to start the interface.

### Manual Verification
1. **Time Skill**: Trigger by speaking "What time is it?" or typing it in. Observe the console log showing `get_time` call and response.
2. **SearXNG Web Search**: Configure your SearXNG URL (e.g. `http://localhost:8080`) and trigger by asking "Search for Rust news". Verify that the SearXNG API is queried successfully and the results are printed.
3. **n8n Workflow**: Configure n8n Webhook URL and say "Run n8n automation for sending email". Verify that the request is successfully dispatched to the endpoint.
4. **Voice Feedback**: Verify Piper TTS reads out responses in real time.
