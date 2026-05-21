# Task: Complete voice/text console agent with skills and premium GUI

- [x] Install system audio dependencies (User task: `sudo dnf install -y alsa-lib-devel`)
- [x] Fix syntax errors and clean up `src/main.rs`
- [x] Implement `Skill` trait, registry, and generic `ActionCall`
- [x] Create `GetTimeSkill`
- [x] Create `WebSearchSkill` utilizing SearXNG JSON API
- [x] Create `N8nTaskSkill` utilizing configurable n8n webhooks
- [x] Upgrade Egui GUI:
  - [x] Add dark theme and custom styled layout
  - [x] Add interactive BMO visualizer glow and mouth waves
  - [x] Add settings panel for Ollama, Model, SearXNG, and n8n URLs
  - [x] Add real-time scrolling conversation history & tool execution logs
  - [x] Add manual typing text box and send button
- [x] Validate compilation and test
