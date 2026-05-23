#![allow(dead_code)]

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use eframe::egui;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

// =========================================================================
// 1. DATA STRUCTURES & CONFIGURATION
// =========================================================================

#[derive(Clone, PartialEq, Debug)]
enum AgentState {
    Idle,
    Listening,
    Thinking,
    Speaking(String),
}

#[derive(Clone, Debug)]
enum LogType {
    User,
    Assistant,
    ToolCall { name: String, args: String },
    ToolResult { name: String, result: String },
    SystemInfo,
    Error(String),
}

#[derive(Clone, Debug)]
struct LogEntry {
    log_type: LogType,
    text: String,
    timestamp: chrono::DateTime<chrono::Local>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct AppConfig {
    server_url: String,
    model_name: String,
    searxng_url: String,
    n8n_url: String,
}

struct AppState {
    config: AppConfig,
    logs: Vec<LogEntry>,
    agent_state: AgentState,
}

enum AiMessage {
    AudioData(Vec<f32>),
    ProcessAudio,
    TextInput(String),
}

// Ollama Chat API Structures
#[derive(Serialize, Clone)]
struct ChatMessage {
    role: String,
    content: String,
}

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    stream: bool,
}

#[derive(Deserialize)]
struct ChatResponse {
    message: ChatMessageResponse,
}

#[derive(Deserialize)]
struct ChatMessageResponse {
    content: String,
}

#[derive(Deserialize, Debug)]
struct GenericActionCall {
    action: String,
    #[serde(flatten)]
    parameters: serde_json::Value,
}

// =========================================================================
// 2. EXTENSIBLE SKILL TRAIT & REGISTERED SKILLS
// =========================================================================

pub trait Skill: Send + Sync {
    /// Unique identifier of the tool (e.g. "get_time", "web_search", "n8n_task")
    fn name(&self) -> &'static str;

    /// The instruction description to present to the LLM in the system prompt.
    fn description(&self) -> &'static str;

    /// Execute the skill logic using arguments parsed from the LLM JSON response.
    fn execute(&self, args: &serde_json::Value, config: &AppConfig) -> Result<String>;
}

// --- SKILL 1: GET TIME ---
struct GetTimeSkill;
impl Skill for GetTimeSkill {
    fn name(&self) -> &'static str {
        "get_time"
    }

    fn description(&self) -> &'static str {
        "To get the current time or date: {\"action\": \"get_time\"}"
    }

    fn execute(&self, _args: &serde_json::Value, _config: &AppConfig) -> Result<String> {
        let time_str = chrono::Local::now()
            .format("%A, %B %e at %l:%M %p")
            .to_string();
        Ok(format!("The current time is {}", time_str))
    }
}

// --- SKILL 2: SEARXNG WEB SEARCH ---
struct WebSearchSkill;
impl Skill for WebSearchSkill {
    fn name(&self) -> &'static str {
        "web_search"
    }

    fn description(&self) -> &'static str {
        "To search the web for information: {\"action\": \"web_search\", \"query\": \"<search query>\"}"
    }

    fn execute(&self, args: &serde_json::Value, config: &AppConfig) -> Result<String> {
        let query = args.get("query")
            .and_then(|v| v.as_str())
            .context("Missing 'query' parameter in web_search call")?;

        let searx_url = if config.searxng_url.is_empty() {
            "http://localhost:8080".to_string()
        } else {
            config.searxng_url.clone()
        };

        let base_url = format!("{}/search", searx_url.trim_end_matches('/'));
        let mut url = reqwest::Url::parse(&base_url).context("Failed to parse SearXNG base URL")?;
        
        // Safely append query parameters with automatic encoding
        url.query_pairs_mut()
            .append_pair("q", query)
            .append_pair("format", "json");

        let client = reqwest::blocking::Client::new();
        let res = client.get(url)
            .send()
            .context("Failed to connect to SearXNG server")?;

        if !res.status().is_success() {
            return Err(anyhow!("SearXNG returned error status code: {}", res.status()));
        }

        let search_res: serde_json::Value = res.json().context("Failed to parse SearXNG JSON response")?;
        let mut results_summary = String::new();

        if let Some(results) = search_res.get("results").and_then(|r| r.as_array()) {
            if results.is_empty() {
                return Ok("No search results were found for this query.".to_string());
            }
            for (i, item) in results.iter().take(4).enumerate() {
                let title = item.get("title").and_then(|t| t.as_str()).unwrap_or("Untitled");
                let content = item.get("content").and_then(|c| c.as_str()).unwrap_or("No snippet available.");
                let item_url = item.get("url").and_then(|u| u.as_str()).unwrap_or("");
                results_summary.push_str(&format!("{}. [{}]({})\n   {}\n\n", i + 1, title, item_url, content));
            }
        } else {
            return Ok("No search results array found in SearXNG response.".to_string());
        }

        Ok(results_summary)
    }
}

// --- SKILL 3: N8N TASK TRIGGER ---
struct N8nTaskSkill;
impl Skill for N8nTaskSkill {
    fn name(&self) -> &'static str {
        "n8n_task"
    }

    fn description(&self) -> &'static str {
        "To trigger a task or automation workflow on n8n (e.g. sending emails, setting calendar events): {\"action\": \"n8n_task\", \"workflow_id\": \"<id or name>\", \"payload\": <JSON object with task details>}"
    }

    fn execute(&self, args: &serde_json::Value, config: &AppConfig) -> Result<String> {
        if config.n8n_url.is_empty() {
            return Err(anyhow!("n8n Webhook URL is not configured in settings!"));
        }

        let workflow_id = args.get("workflow_id")
            .and_then(|v| v.as_str())
            .unwrap_or("default");
        let payload = args.get("payload")
            .unwrap_or(&serde_json::Value::Null);

        let client = reqwest::blocking::Client::new();
        let res = client.post(&config.n8n_url)
            .json(&serde_json::json!({
                "workflow_id": workflow_id,
                "payload": payload,
                "timestamp": chrono::Utc::now().to_rfc3339()
            }))
            .send()
            .context("Failed to dispatch request to n8n webhook")?;

        let status = res.status();
        let body = res.text().unwrap_or_default();

        Ok(format!("n8n webhook triggered! Status: {}. Response: {}", status, body))
    }
}

// --- SKILL 4: WEATHER ---
struct WeatherSkill;
impl Skill for WeatherSkill {
    fn name(&self) -> &'static str {
        "get_weather"
    }

    fn description(&self) -> &'static str {
        "To get current weather conditions for a city/location: {\"action\": \"get_weather\", \"location\": \"<city name>\"}"
    }

    fn execute(&self, args: &serde_json::Value, _config: &AppConfig) -> Result<String> {
        let location = args.get("location")
            .and_then(|v| v.as_str())
            .unwrap_or("Sydney");

        let url = format!("https://wttr.in/{}?format=j1", location);
        let client = reqwest::blocking::Client::new();
        let res = client.get(&url)
            .send()
            .context("Failed to connect to weather service")?;

        if !res.status().is_success() {
            return Err(anyhow!("Weather service returned status code: {}", res.status()));
        }

        let weather_res: serde_json::Value = res.json().context("Failed to parse weather JSON")?;
        let temp = weather_res.pointer("/current_condition/0/temp_C")
            .and_then(|t| t.as_str())
            .context("Failed to parse temperature")?;
        let desc = weather_res.pointer("/current_condition/0/weatherDesc/0/value")
            .and_then(|d| d.as_str())
            .context("Failed to parse weather description")?;
        let feel = weather_res.pointer("/current_condition/0/FeelsLikeC")
            .and_then(|f| f.as_str())
            .unwrap_or(temp);

        Ok(format!("The current weather in {} is {}, with a temp of {}°C (feels like {}°C).", location, desc, temp, feel))
    }
}

// --- SKILL 5: PLAY MUSIC (PLAYERCTL) ---
struct PlayMusicSkill;
impl Skill for PlayMusicSkill {
    fn name(&self) -> &'static str {
        "play_music"
    }

    fn description(&self) -> &'static str {
        "To control system music playback (play, pause, next, previous): {\"action\": \"play_music\", \"playback_action\": \"play|pause|next|previous\"}"
    }

    fn execute(&self, args: &serde_json::Value, _config: &AppConfig) -> Result<String> {
        let action = args.get("playback_action")
            .and_then(|v| v.as_str())
            .context("Missing 'playback_action' parameter in play_music call")?;

        let valid_action = match action {
            "play" | "pause" | "next" | "previous" => action,
            "skip" => "next",
            "stop" => "pause",
            _ => return Err(anyhow!("Invalid playback action: {}. Supported: play, pause, next, previous", action)),
        };

        let mut cmd = Command::new("playerctl");
        cmd.arg(valid_action);

        let output = cmd.output().context("Failed to execute playerctl command. Is playerctl installed?")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!("playerctl failed: {}", stderr.trim()));
        }

        Ok(format!("Music playback command dispatched successfully: {}", valid_action))
    }
}

// --- SKILL 6: SUB-AGENT COGNITIVE PATTERN ---
struct SubAgentSkill {
    app_state: Arc<Mutex<AppState>>,
}

impl SubAgentSkill {
    fn new(app_state: Arc<Mutex<AppState>>) -> Self {
        Self { app_state }
    }
}

impl Skill for SubAgentSkill {
    fn name(&self) -> &'static str {
        "run_sub_task"
    }

    fn description(&self) -> &'static str {
        "To delegate a complex multi-step task or research to a focused sub-agent: {\"action\": \"run_sub_task\", \"task\": \"<detailed task description>\"}"
    }

    fn execute(&self, args: &serde_json::Value, config: &AppConfig) -> Result<String> {
        let task = args.get("task")
            .and_then(|v| v.as_str())
            .context("Missing 'task' parameter in run_sub_task call")?;

        log_message(
            &self.app_state,
            LogType::SystemInfo,
            &format!("🧠 Sub-Agent spawned to solve task: \"{}\"", task),
        );

        let summary = run_sub_agent_loop(&self.app_state, config, task)?;
        Ok(summary)
    }
}

fn run_sub_agent_loop(
    app_state: &Arc<Mutex<AppState>>,
    config: &AppConfig,
    task: &str,
) -> Result<String> {
    let client = reqwest::blocking::Client::new();

    let sub_system_prompt = "\
        You are a focused research sub-agent. Your goal is to solve the following user task:\n\n\
        USER TASK: \"[TASK_GOAL]\"\n\n\
        You must analyze the task step-by-step. You have access to the following tools to gather information:\n\
        - To search the web: {\"action\": \"web_search\", \"query\": \"<query>\"}\n\
        - To get current time: {\"action\": \"get_time\"}\n\n\
        RULES:\n\
        1. Keep your reasoning efficient. Solve the task in the minimum number of steps.\n\
        2. To use a tool, reply ONLY with a raw JSON object and no other text.\n\
        3. If you have gathered all necessary information and are ready to provide a final consolidated report to the parent agent, you MUST reply ONLY with a JSON object calling the 'done' action:\n\
           {\"action\": \"done\", \"summary\": \"<detailed conversational summary and answer to the user's task>\"}\n\n\
        Stay focused and complete the task.";

    let system_content = sub_system_prompt.replace("[TASK_GOAL]", task);

    let mut sub_messages = vec![ChatMessage {
        role: "system".to_string(),
        content: system_content,
    }];

    // Create a local mini-registry for the sub-agent
    let mut sub_registry = std::collections::HashMap::new();
    sub_registry.insert("web_search".to_string(), Box::new(WebSearchSkill) as Box<dyn Skill>);
    sub_registry.insert("get_time".to_string(), Box::new(GetTimeSkill) as Box<dyn Skill>);

    let max_iterations = 4;
    let mut current_iteration = 0;

    while current_iteration < max_iterations {
        current_iteration += 1;

        log_message(
            app_state,
            LogType::SystemInfo,
            &format!("🧠 Sub-Agent Step {}/{}", current_iteration, max_iterations),
        );

        let request_body = ChatRequest {
            model: config.model_name.clone(),
            messages: sub_messages.clone(),
            stream: false,
        };

        let res = client.post(&config.server_url)
            .json(&request_body)
            .send()
            .context("Sub-agent failed to connect to Ollama server")?;

        if !res.status().is_success() {
            return Err(anyhow!("Sub-agent Ollama server returned error status: {}", res.status()));
        }

        let chat_res: ChatResponse = res.json().context("Failed to parse sub-agent Ollama response JSON")?;
        let raw_reply = chat_res.message.content.trim();
        let cleaned_reply = clean_json_string(raw_reply);

        if let Ok(action_call) = serde_json::from_str::<GenericActionCall>(&cleaned_reply) {
            let tool_name = action_call.action.clone();
            
            if tool_name == "done" {
                let summary = action_call.parameters.get("summary")
                    .and_then(|s| s.as_str())
                    .unwrap_or(raw_reply);
                
                log_message(
                    app_state,
                    LogType::SystemInfo,
                    &format!("🧠 Sub-Agent successfully completed task!"),
                );
                return Ok(summary.to_string());
            }

            if let Some(sub_skill) = sub_registry.get(&tool_name) {
                let tool_args = action_call.parameters.to_string();

                log_message(
                    app_state,
                    LogType::ToolCall {
                        name: format!("sub-agent::{}", tool_name),
                        args: tool_args,
                    },
                    &format!("🧠 Sub-Agent triggering skill '{}'...", tool_name),
                );

                let skill_result = sub_skill.execute(&action_call.parameters, config);

                match skill_result {
                    Ok(result_str) => {
                        log_message(
                            app_state,
                            LogType::ToolResult {
                                name: format!("sub-agent::{}", tool_name),
                                result: result_str.clone(),
                            },
                            &format!("🧠 Sub-Agent skill '{}' completed.", tool_name),
                        );

                        sub_messages.push(ChatMessage {
                            role: "assistant".to_string(),
                            content: raw_reply.to_string(),
                        });

                        sub_messages.push(ChatMessage {
                            role: "user".to_string(),
                            content: format!("System tool observation: {}", result_str),
                        });
                    }
                    Err(e) => {
                        log_message(
                            app_state,
                            LogType::Error(format!("🧠 Sub-Agent skill '{}' failed: {}", tool_name, e)),
                            &format!("🧠 Sub-Agent skill '{}' failed.", tool_name),
                        );

                        sub_messages.push(ChatMessage {
                            role: "assistant".to_string(),
                            content: raw_reply.to_string(),
                        });

                        sub_messages.push(ChatMessage {
                            role: "user".to_string(),
                            content: format!("System tool error: {}", e),
                        });
                    }
                }
            } else {
                sub_messages.push(ChatMessage {
                    role: "assistant".to_string(),
                    content: raw_reply.to_string(),
                });
                sub_messages.push(ChatMessage {
                    role: "user".to_string(),
                    content: format!("Tool '{}' is not available in sub-agent scope. You can only call 'web_search', 'get_time', or 'done'.", tool_name),
                });
            }
        } else {
            log_message(
                app_state,
                LogType::SystemInfo,
                &format!("🧠 Sub-Agent completed task with plain reply."),
            );
            return Ok(raw_reply.to_string());
        }
    }

    log_message(
        app_state,
        LogType::SystemInfo,
        &format!("🧠 Sub-Agent reached safety limit of {} steps. Merging findings.", max_iterations),
    );

    sub_messages.push(ChatMessage {
        role: "user".to_string(),
        content: "You have reached the maximum steps. Provide the best possible consolidated summary of your research so far.".to_string(),
    });

    let request_body = ChatRequest {
        model: config.model_name.clone(),
        messages: sub_messages,
        stream: false,
    };

    if let Ok(res) = client.post(&config.server_url).json(&request_body).send() {
        if let Ok(chat_res) = res.json::<ChatResponse>() {
            return Ok(chat_res.message.content.trim().to_string());
        }
    }

    Ok("Sub-agent reached reasoning limit but was unable to compile a final summary.".to_string())
}

// --- SKILL REGISTRY CONTAINER ---
struct SkillRegistry {
    skills: std::collections::HashMap<String, Box<dyn Skill>>,
}

impl SkillRegistry {
    fn new(app_state: Arc<Mutex<AppState>>) -> Self {
        let mut registry = Self {
            skills: std::collections::HashMap::new(),
        };
        registry.register(Box::new(GetTimeSkill));
        registry.register(Box::new(WebSearchSkill));
        registry.register(Box::new(N8nTaskSkill));
        registry.register(Box::new(WeatherSkill));
        registry.register(Box::new(PlayMusicSkill));
        registry.register(Box::new(SubAgentSkill::new(app_state)));
        registry
    }

    fn register(&mut self, skill: Box<dyn Skill>) {
        self.skills.insert(skill.name().to_string(), skill);
    }

    fn get(&self, name: &str) -> Option<&Box<dyn Skill>> {
        self.skills.get(name)
    }

    fn generate_system_prompt(&self) -> String {
        let mut prompt = String::from(
            "You are BMO, a helpful voice assistant. Keep your answers brief, conversational, and user-friendly.\n\
            You have access to tools. To use a tool, you MUST reply ONLY with a raw JSON object and no other text.\n\
            Available tools:\n"
        );
        for skill in self.skills.values() {
            prompt.push_str(&format!("- {}\n", skill.description()));
        }
        prompt.push_str("If you do not need a tool, just answer normally.");
        prompt
    }
}

// =========================================================================
// 3. HELPER FUNCTIONS & VOICE SYSTEM
// =========================================================================

fn clean_json_string(raw: &str) -> String {
    let mut cleaned = raw.trim();
    if cleaned.starts_with("```json") {
        cleaned = cleaned.trim_start_matches("```json");
    } else if cleaned.starts_with("```") {
        cleaned = cleaned.trim_start_matches("```");
    }
    if cleaned.ends_with("```") {
        cleaned = cleaned.trim_end_matches("```");
    }
    cleaned.trim().to_string()
}

fn speak(text: &str, app_state: &Arc<Mutex<AppState>>) {
    println!("🔊 Speaking: {}", text);
    let piper = Command::new("piper-tts")
        .arg("--model")
        .arg("./bmo.onnx")
        .arg("--output_raw")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn();

    let mut piper_process = match piper {
        Ok(p) => p,
        Err(e) => {
            log_message(
                app_state,
                LogType::Error(format!("Failed to start piper-tts: {}", e)),
                "Speech output failed: could not launch piper-tts",
            );
            eprintln!("Failed to start piper-tts. Is it installed? Error: {}", e);
            return;
        }
    };

    if let Some(mut piper_stdin) = piper_process.stdin.take() {
        if let Err(e) = piper_stdin.write_all(text.as_bytes()) {
            log_message(
                app_state,
                LogType::Error(format!("Failed to write text to piper-tts: {}", e)),
                "Speech output failed while streaming text to piper-tts",
            );
            eprintln!("Failed to write text to Piper: {}", e);
            return;
        }
    }

    let piper_output = match piper_process.wait_with_output() {
        Ok(out) => out,
        Err(e) => {
            log_message(
                app_state,
                LogType::Error(format!("Failed to wait for piper-tts output: {}", e)),
                "Speech output failed while waiting for piper-tts",
            );
            eprintln!("Failed to wait on Piper execution: {}", e);
            return;
        }
    };

    if !piper_output.status.success() {
        let stderr = String::from_utf8_lossy(&piper_output.stderr).trim().to_string();
        log_message(
            app_state,
            LogType::Error(format!("piper-tts exited with status {}", piper_output.status)),
            &format!(
                "Speech output failed: piper-tts returned {}{}",
                piper_output.status,
                if stderr.is_empty() {
                    "".to_string()
                } else {
                    format!(" ({})", stderr)
                }
            ),
        );
        return;
    }

    let aplay = Command::new("aplay")
        .arg("-r")
        .arg("22050")
        .arg("-f")
        .arg("S16_LE")
        .arg("-t")
        .arg("raw")
        .arg("-c")
        .arg("1")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();

    let mut aplay_process = match aplay {
        Ok(a) => a,
        Err(e) => {
            log_message(
                app_state,
                LogType::Error(format!("Failed to start aplay: {}", e)),
                "Speech output failed: could not launch aplay",
            );
            eprintln!("Failed to start aplay. Is it installed? Error: {}", e);
            return;
        }
    };

    if let Some(mut aplay_stdin) = aplay_process.stdin.take() {
        if let Err(e) = aplay_stdin.write_all(&piper_output.stdout) {
            log_message(
                app_state,
                LogType::Error(format!("Failed to pipe audio to aplay: {}", e)),
                "Speech output failed while piping audio to aplay",
            );
            eprintln!("Failed to pipe audio buffer to aplay: {}", e);
        }
    }

    match aplay_process.wait() {
        Ok(status) => {
            if !status.success() {
                log_message(
                    app_state,
                    LogType::Error(format!("aplay exited with status {}", status)),
                    &format!("Speech output failed: aplay returned {}", status),
                );
            }
        }
        Err(e) => {
            log_message(
                app_state,
                LogType::Error(format!("Failed waiting on aplay: {}", e)),
                "Speech output failed while waiting for aplay",
            );
        }
    }
}

fn log_message(app_state: &Arc<Mutex<AppState>>, log_type: LogType, text: &str) {
    if let Ok(mut state) = app_state.lock() {
        state.logs.push(LogEntry {
            log_type,
            text: text.to_string(),
            timestamp: chrono::Local::now(),
        });
    }
}

fn command_in_path(command: &str) -> bool {
    let Some(path_var) = std::env::var_os("PATH") else {
        return false;
    };

    std::env::split_paths(&path_var).any(|path| path.join(command).exists())
}

fn run_startup_checks(app_state: &Arc<Mutex<AppState>>) {
    let required_files = ["ggml-tiny.en.bin", "bmo.onnx", "bmo.onnx.json"];
    for file in required_files {
        if Path::new(file).exists() {
            log_message(
                app_state,
                LogType::SystemInfo,
                &format!("Startup check: Found required file '{}'", file),
            );
        } else {
            log_message(
                app_state,
                LogType::Error(format!("Missing required file: {}", file)),
                &format!("Startup check failed: Missing required file '{}'", file),
            );
        }
    }

    let required_commands = ["piper-tts", "aplay", "playerctl"];
    for cmd in required_commands {
        if command_in_path(cmd) {
            log_message(
                app_state,
                LogType::SystemInfo,
                &format!("Startup check: Found command '{}'", cmd),
            );
        } else {
            log_message(
                app_state,
                LogType::Error(format!("Command '{}' was not found in PATH", cmd)),
                &format!("Startup warning: '{}' is not installed or not in PATH", cmd),
            );
        }
    }

    let host = cpal::default_host();
    if host.default_input_device().is_some() {
        log_message(
            app_state,
            LogType::SystemInfo,
            "Startup check: Microphone input device detected",
        );
    } else {
        log_message(
            app_state,
            LogType::Error("No microphone input device was detected".to_string()),
            "Startup warning: No default microphone input device detected",
        );
    }

    let server_url = if let Ok(state) = app_state.lock() {
        state.config.server_url.clone()
    } else {
        String::new()
    };

    if server_url.is_empty() {
        log_message(
            app_state,
            LogType::Error("Ollama URL is empty".to_string()),
            "Startup warning: Ollama URL is empty; open Settings and configure it",
        );
        return;
    }

    let health_url = if server_url.ends_with("/api/chat") {
        server_url.replace("/api/chat", "/api/tags")
    } else {
        format!("{}/api/tags", server_url.trim_end_matches('/'))
    };

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(2))
        .build();

    match client {
        Ok(client) => match client.get(&health_url).send() {
            Ok(res) => {
                if res.status().is_success() {
                    log_message(
                        app_state,
                        LogType::SystemInfo,
                        &format!("Startup check: Ollama reachable at {}", health_url),
                    );
                } else {
                    log_message(
                        app_state,
                        LogType::Error(format!(
                            "Ollama health check failed with status {}",
                            res.status()
                        )),
                        &format!(
                            "Startup warning: Ollama responded with {} at {}",
                            res.status(),
                            health_url
                        ),
                    );
                }
            }
            Err(e) => {
                log_message(
                    app_state,
                    LogType::Error(format!("Could not reach Ollama health endpoint: {}", e)),
                    &format!(
                        "Startup warning: Could not reach Ollama at {}",
                        health_url
                    ),
                );
            }
        },
        Err(e) => {
            log_message(
                app_state,
                LogType::Error(format!("Could not build HTTP client for startup checks: {}", e)),
                "Startup warning: HTTP client creation failed during Ollama preflight",
            );
        }
    }
}

// =========================================================================
// 4. GUI IMPLEMENTATION (Egui v0.34 Adopted)
// =========================================================================

struct AgentApp {
    state: Arc<Mutex<AppState>>,
    manual_input: String,
    tx: Sender<AiMessage>,
    first_run: bool,
}

impl eframe::App for AgentApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        if self.first_run {
            let mut visuals = egui::Visuals::dark();
            visuals.widgets.noninteractive.bg_fill = egui::Color32::from_rgb(18, 18, 22);
            visuals.widgets.noninteractive.fg_stroke.color = egui::Color32::from_rgb(200, 200, 200);
            visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(28, 28, 36);
            visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(38, 38, 48);
            visuals.widgets.active.bg_fill = egui::Color32::from_rgb(48, 48, 62);
            ctx.set_visuals(visuals);
            self.first_run = false;
        }

        let time = ctx.input(|i| i.time);

        // Lock AppState for UI drawing
        if let Ok(mut state) = self.state.lock() {
            let current_state = state.agent_state.clone();
            
            // LEFT PANEL: Face visualizer and quick guide (Egui v0.34 compliant)
            egui::Panel::left("visualizer_panel")
                .resizable(false)
                .default_size(320.0)
                .show_inside(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.add_space(20.0);
                        ui.heading(
                            egui::RichText::new("👾 BMO AGENT")
                                .strong()
                                .color(egui::Color32::from_rgb(0, 230, 115))
                                .size(24.0)
                        );
                        ui.add_space(15.0);

                        // Visualizer Screen Frame
                        let size = egui::vec2(280.0, 240.0);
                        let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                        let center = rect.center();
                        let painter = ui.painter_at(rect);

                        // Base layout dimensions
                        let eye_spacing = 80.0;
                        let eye_y_offset = -20.0;
                        let mouth_y_offset = 35.0;

                        // Dynamics depending on AgentState
                        let blink_scale;
                        let look_offset;
                        let smile_curve;
                        let mouth_openness;
                        let mouth_w;
                        let face_color;
                        let text_label;
                        let glow_radius;

                        match &current_state {
                            AgentState::Idle => {
                                blink_scale = if (time % 5.0) < 0.15 { 0.1 } else { 1.0 };
                                look_offset = egui::Vec2::ZERO;
                                smile_curve = 10.0;
                                mouth_openness = 0.0;
                                mouth_w = 60.0 + 5.0 * (time * 1.2).sin() as f32;
                                face_color = egui::Color32::from_rgb(120, 130, 160);
                                text_label = "IDLE";
                                glow_radius = 110.0 + 5.0 * (time * 1.5).sin() as f32;
                            }
                            AgentState::Listening => {
                                blink_scale = 1.0;
                                look_offset = egui::Vec2::ZERO;
                                smile_curve = 0.0;
                                mouth_openness = 12.0 + 4.0 * (time * 8.0).sin() as f32;
                                mouth_w = 30.0;
                                face_color = egui::Color32::from_rgb(0, 200, 255);
                                text_label = "LISTENING";
                                glow_radius = 120.0 + 10.0 * (time * 8.0).cos().abs() as f32;
                            }
                            AgentState::Thinking => {
                                blink_scale = if (time % 5.0) < 0.15 { 0.1 } else { 1.0 };
                                look_offset = egui::vec2(15.0 * (time * 3.0).sin() as f32, -5.0);
                                smile_curve = -5.0;
                                mouth_openness = 0.0;
                                mouth_w = 40.0;
                                face_color = egui::Color32::from_rgb(255, 170, 0);
                                text_label = "THINKING";
                                glow_radius = 115.0 + 6.0 * (time * 4.0).sin() as f32;
                            }
                            AgentState::Speaking(_) => {
                                blink_scale = if (time % 5.0) < 0.15 { 0.1 } else { 1.0 };
                                look_offset = egui::Vec2::ZERO;
                                let talk_wave = (time * 25.0).sin().abs() as f32 * 0.8
                                    + (time * 12.0).cos().abs() as f32 * 0.2;
                                smile_curve = 8.0;
                                mouth_openness = 4.0 + 25.0 * talk_wave;
                                mouth_w = 55.0 + 10.0 * (time * 6.0).cos() as f32;
                                face_color = egui::Color32::from_rgb(0, 230, 115);
                                text_label = "SPEAKING";
                                glow_radius = 120.0 + 12.0 * talk_wave;
                            }
                        }

                        // Draw screen background & outline using version-stable overlapping filled rectangles
                        let screen_rect = rect.shrink(5.0);
                        painter.rect_filled(screen_rect, 15.0, face_color);
                        let inner_screen_rect = screen_rect.shrink(2.5);
                        painter.rect_filled(inner_screen_rect, 13.0, egui::Color32::from_rgb(15, 20, 28));

                        // Glowing inner aura
                        let glow_color = egui::Color32::from_rgba_unmultiplied(
                            face_color.r(),
                            face_color.g(),
                            face_color.b(),
                            12,
                        );
                        painter.circle_filled(center, glow_radius, glow_color);

                        // Draw Eyes (using cross-version stable circle_filled)
                        let left_eye_center = center + egui::vec2(-eye_spacing / 2.0, eye_y_offset) + look_offset;
                        let right_eye_center = center + egui::vec2(eye_spacing / 2.0, eye_y_offset) + look_offset;
                        let eye_radius = 13.0;

                        painter.circle_filled(left_eye_center, eye_radius * blink_scale, face_color);
                        painter.circle_filled(right_eye_center, eye_radius * blink_scale, face_color);

                        // Draw Mouth
                        let mouth_center = center + egui::vec2(0.0, mouth_y_offset);
                        if mouth_openness <= 2.0 {
                            let p0 = mouth_center + egui::vec2(-mouth_w / 2.0, 0.0);
                            let p2 = mouth_center + egui::vec2(mouth_w / 2.0, 0.0);
                            let p1 = mouth_center + egui::vec2(0.0, smile_curve);

                            let mut points = vec![];
                            for i in 0..=10 {
                                let t = i as f32 / 10.0;
                                let u = 1.0 - t;
                                let x = u * u * p0.x + 2.0 * u * t * p1.x + t * t * p2.x;
                                let y = u * u * p0.y + 2.0 * u * t * p1.y + t * t * p2.y;
                                points.push(egui::pos2(x, y));
                            }
                            painter.add(egui::epaint::PathShape::line(
                                points,
                                egui::Stroke::new(8.0, face_color),
                            ));
                        } else {
                            // Rounded capsule rectangle represents open mouth perfectly in all Egui versions
                            let mouth_rect = egui::Rect::from_center_size(
                                mouth_center + egui::vec2(0.0, smile_curve / 2.0),
                                egui::vec2(mouth_w, mouth_openness * 2.0)
                            );
                            painter.rect_filled(mouth_rect, mouth_openness, face_color);
                        }

                        ui.add_space(20.0);

                        // Dynamic status chip
                        let chip_bg = match &current_state {
                            AgentState::Idle => egui::Color32::from_rgb(30, 35, 45),
                            AgentState::Listening => egui::Color32::from_rgb(0, 50, 80),
                            AgentState::Thinking => egui::Color32::from_rgb(80, 50, 0),
                            AgentState::Speaking(_) => egui::Color32::from_rgb(0, 60, 30),
                        };

                        egui::Frame::NONE
                            .fill(chip_bg)
                            .corner_radius(14)
                            .inner_margin(egui::Margin::symmetric(24, 8))
                            .show(ui, |ui| {
                                ui.label(
                                    egui::RichText::new(text_label)
                                        .strong()
                                        .color(face_color)
                                        .size(15.0)
                                );
                            });

                        ui.add_space(25.0);
                        ui.separator();
                        ui.add_space(20.0);

                        // Help details
                        ui.label(egui::RichText::new("Voice Operations:").strong().color(egui::Color32::GRAY));
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new("🎙 Active mic: Start speaking\n⏱ Pauses trigger LLM processing\n💬 Type in textbox below to chat")
                                .weak()
                                .size(11.5)
                        );
                    });
                });

            // TOP PANEL: Server & URL Configuration (Collapsible)
            egui::Panel::top("top_settings_panel")
                .show_inside(ui, |ui| {
                    ui.add_space(6.0);
                    ui.collapsing("⚙ Settings & Server Configuration", |ui| {
                        ui.add_space(4.0);
                        egui::Grid::new("settings_grid")
                            .num_columns(2)
                            .spacing([12.0, 8.0])
                            .striped(true)
                            .show(ui, |ui| {
                                ui.label("Ollama URL:");
                                ui.add(egui::TextEdit::singleline(&mut state.config.server_url).desired_width(450.0));
                                ui.end_row();

                                ui.label("Model Name:");
                                ui.add(egui::TextEdit::singleline(&mut state.config.model_name).desired_width(220.0));
                                ui.end_row();

                                ui.label("SearXNG URL:");
                                ui.add(egui::TextEdit::singleline(&mut state.config.searxng_url).desired_width(450.0));
                                ui.end_row();

                                ui.label("n8n Webhook URL:");
                                ui.add(egui::TextEdit::singleline(&mut state.config.n8n_url).desired_width(450.0));
                                ui.end_row();
                            });
                        ui.add_space(6.0);
                    });
                    ui.add_space(4.0);
                });

            // BOTTOM PANEL: Text Chat Box
            egui::Panel::bottom("bottom_input_panel")
                .show_inside(ui, |ui| {
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        let text_edit = egui::TextEdit::singleline(&mut self.manual_input)
                            .hint_text("Type a message or ask BMO to trigger a skill...")
                            .desired_width(ui.available_width() - 160.0);

                        let response = ui.add(text_edit);
                        let is_enter = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));

                        let send_btn = ui.add(
                            egui::Button::new(egui::RichText::new("Send 🚀").strong())
                                .min_size(egui::vec2(75.0, 30.0))
                        );

                        if (is_enter || send_btn.clicked()) && !self.manual_input.trim().is_empty() {
                            let text = self.manual_input.trim().to_string();
                            let _ = self.tx.send(AiMessage::TextInput(text));
                            self.manual_input.clear();
                        }

                        if ui.button("Clear Logs").clicked() {
                            state.logs.clear();
                            state.logs.push(LogEntry {
                                log_type: LogType::SystemInfo,
                                text: "Logs cleared.".to_string(),
                                timestamp: chrono::Local::now(),
                            });
                        }
                    });
                    ui.add_space(8.0);
                });

            // CENTRAL PANEL: Scrollable Live Logs drawn directly inside `ui`
            ui.vertical(|ui| {
                ui.add_space(4.0);
                ui.heading("📝 Live Activity & Conversation Logs");
                ui.add_space(6.0);

                egui::ScrollArea::vertical()
                    .auto_shrink([false; 2])
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        for entry in &state.logs {
                            draw_log_card(ui, entry);
                        }
                    });
            });
        }

        // Keep UI animating for continuous face visualization breathing/mouth movements
        ctx.request_repaint();
    }
}

fn draw_log_card(ui: &mut egui::Ui, entry: &LogEntry) {
    let (bg_color, stroke_color, fg_color, icon, title) = match &entry.log_type {
        LogType::User => (
            egui::Color32::from_rgba_unmultiplied(0, 150, 255, 22),
            egui::Color32::from_rgb(0, 150, 255),
            egui::Color32::from_rgb(205, 235, 255),
            "👤",
            "You",
        ),
        LogType::Assistant => (
            egui::Color32::from_rgba_unmultiplied(0, 230, 115, 22),
            egui::Color32::from_rgb(0, 230, 115),
            egui::Color32::from_rgb(210, 255, 230),
            "🤖",
            "BMO Agent",
        ),
        LogType::ToolCall { name, .. } => (
            egui::Color32::from_rgba_unmultiplied(255, 170, 0, 18),
            egui::Color32::from_rgb(255, 170, 0),
            egui::Color32::from_rgb(255, 230, 180),
            "⚙️",
            name.as_str(),
        ),
        LogType::ToolResult { name, .. } => (
            egui::Color32::from_rgba_unmultiplied(255, 170, 0, 12),
            egui::Color32::from_rgb(210, 150, 0),
            egui::Color32::from_rgb(240, 225, 190),
            "📊",
            name.as_str(),
        ),
        LogType::SystemInfo => (
            egui::Color32::from_rgba_unmultiplied(150, 150, 150, 18),
            egui::Color32::from_rgb(160, 160, 160),
            egui::Color32::from_rgb(225, 225, 225),
            "ℹ️",
            "System Information",
        ),
        LogType::Error(_) => (
            egui::Color32::from_rgba_unmultiplied(255, 50, 50, 22),
            egui::Color32::from_rgb(255, 60, 60),
            egui::Color32::from_rgb(255, 215, 215),
            "⚠️",
            "Error",
        ),
    };

    egui::Frame::NONE
        .fill(bg_color)
        .stroke(egui::Stroke::new(1.0, stroke_color))
        .corner_radius(8)
        .inner_margin(egui::Margin::symmetric(8, 8))
        .outer_margin(egui::Margin::symmetric(0, 4))
        .show(ui, |ui| {
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(format!("{} {}", icon, title)).strong().color(stroke_color));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new(entry.timestamp.format("%H:%M:%S").to_string())
                                .weak()
                                .size(10.0)
                        );
                    });
                });
                ui.add_space(2.0);
                ui.label(egui::RichText::new(&entry.text).color(fg_color));
            });
        });
}

// =========================================================================
// 5. MAIN ENTRY & BACKGROUND AI THREAD
// =========================================================================

fn main() -> Result<(), eframe::Error> {
    let shared_state = Arc::new(Mutex::new(AppState {
        config: AppConfig {
            server_url: "http://10.0.0.32:11434/api/chat".to_string(),
            model_name: "gemma4:e4b".to_string(),
            searxng_url: "http://localhost:8080".to_string(),
            n8n_url: "".to_string(),
        },
        logs: vec![LogEntry {
            log_type: LogType::SystemInfo,
            text: "System loaded. BMO is ready to help!".to_string(),
            timestamp: chrono::Local::now(),
        }],
        agent_state: AgentState::Idle,
    }));

    run_startup_checks(&shared_state);

    let (ai_tx, ai_rx) = mpsc::channel::<AiMessage>();

    let ai_state = shared_state.clone();
    let ai_tx_for_audio = ai_tx.clone();

    // Spawn AI Pipeline Background thread
    thread::spawn(move || {
        let pipeline_state = ai_state.clone();
        if let Err(e) = run_ai_pipeline(pipeline_state, ai_rx, ai_tx_for_audio) {
            log_message(
                &ai_state,
                LogType::Error(format!("AI pipeline crashed: {}", e)),
                &format!("AI pipeline stopped: {}", e),
            );
            eprintln!("AI Pipeline crashed: {}", e);
        }
    });

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1150.0, 720.0])
            .with_min_inner_size([850.0, 500.0]),
        ..Default::default()
    };

    let app_state = shared_state.clone();
    eframe::run_native(
        "BMO Console Agent",
        options,
        Box::new(move |_cc| {
            Ok(Box::new(AgentApp {
                state: app_state,
                manual_input: String::new(),
                tx: ai_tx,
                first_run: true,
            }))
        }),
    )
}

fn run_ai_pipeline(
    app_state: Arc<Mutex<AppState>>,
    audio_rx: Receiver<AiMessage>,
    audio_tx: Sender<AiMessage>,
) -> Result<()> {
    let mut whisper_ctx = None;
    let mut whisper_state = None;

    println!("🤖 Loading Whisper Model...");
    let ctx_params = WhisperContextParameters::default();
    match WhisperContext::new_with_params("ggml-tiny.en.bin", ctx_params) {
        Ok(ctx) => {
            let context_stored = whisper_ctx.insert(ctx);
            match context_stored.create_state() {
                Ok(state) => {
                    whisper_state = Some(state);
                    println!("🤖 Whisper Model Loaded Successfully.");
                }
                Err(e) => {
                    let warn_msg = format!("Failed to create Whisper state: {}. Voice transcription will be disabled.", e);
                    eprintln!("⚠️ {}", warn_msg);
                    log_message(&app_state, LogType::Error(warn_msg.clone()), &warn_msg);
                }
            }
        }
        Err(e) => {
            let warn_msg = format!("Failed to load Whisper model: {}. Voice transcription will be disabled.", e);
            eprintln!("⚠️ {}", warn_msg);
            log_message(&app_state, LogType::Error(warn_msg.clone()), &warn_msg);
        }
    }

    let mut _stream = None;

    let host = cpal::default_host();
    match host.default_input_device() {
        Some(device) => {
            let stream_config = cpal::StreamConfig {
                channels: 1,
                sample_rate: 16000,
                buffer_size: cpal::BufferSize::Default,
            };

            let audio_tx_clone = audio_tx.clone();
            let app_state_audio = app_state.clone();

            let mut is_recording = false;
            let mut silence_frames = 0;
            let silence_threshold = 16000;

            match device.build_input_stream(
                &stream_config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    // Bypass input checks if BMO is currently playing audio TTS
                    let is_speaking = {
                        if let Ok(state) = app_state_audio.try_lock() {
                            matches!(state.agent_state, AgentState::Speaking(_))
                        } else {
                            false
                        }
                    };

                    if is_speaking {
                        return;
                    }

                    let mut sum_squares = 0.0;
                    for &sample in data {
                        sum_squares += sample * sample;
                    }
                    let rms = (sum_squares / data.len() as f32).sqrt();

                    if rms > 0.015 {
                        if !is_recording {
                            if let Ok(mut state) = app_state_audio.lock() {
                                state.agent_state = AgentState::Listening;
                            }
                            is_recording = true;
                        }
                        silence_frames = 0;
                    } else if is_recording {
                        silence_frames += data.len();
                    }

                    if is_recording {
                        let _ = audio_tx_clone.send(AiMessage::AudioData(data.to_vec()));
                    }

                    if is_recording && silence_frames > silence_threshold {
                        is_recording = false;
                        silence_frames = 0;
                        let _ = audio_tx_clone.send(AiMessage::ProcessAudio);
                    }
                },
                |err| eprintln!("Audio stream error: {}", err),
                None,
            ) {
                Ok(s) => {
                    match s.play() {
                        Ok(_) => {
                            _stream = Some(s);
                            println!("🎧 Audio Listener Stream Active.");
                            log_message(
                                &app_state,
                                LogType::SystemInfo,
                                "Microphone audio listener stream successfully started.",
                            );
                        }
                        Err(e) => {
                            let warn_msg = format!("Failed to start playing audio stream: {}. Voice input will be disabled.", e);
                            eprintln!("⚠️ {}", warn_msg);
                            log_message(&app_state, LogType::Error(warn_msg.clone()), &warn_msg);
                        }
                    }
                }
                Err(e) => {
                    let warn_msg = format!("Failed to build input audio stream: {}. Voice input will be disabled.", e);
                    eprintln!("⚠️ {}", warn_msg);
                    log_message(&app_state, LogType::Error(warn_msg.clone()), &warn_msg);
                }
            }
        }
        None => {
            let warn_msg = "No microphone found or ALSA failed to identify default input device. Voice input will be disabled.".to_string();
            eprintln!("⚠️ {}", warn_msg);
            log_message(&app_state, LogType::Error(warn_msg.clone()), &warn_msg);
        }
    }

    // Skill system setup
    let registry = SkillRegistry::new(app_state.clone());
    let system_prompt = registry.generate_system_prompt();

    let mut messages = vec![ChatMessage {
        role: "system".to_string(),
        content: system_prompt.clone(),
    }];

    let client = reqwest::blocking::Client::new();
    let max_context_messages = 11;
    let mut audio_buffer: Vec<f32> = Vec::new();

    for message in audio_rx {
        match message {
            AiMessage::AudioData(data) => {
                audio_buffer.extend(data);
            }
            AiMessage::ProcessAudio => {
                {
                    if let Ok(mut state) = app_state.lock() {
                        state.agent_state = AgentState::Thinking;
                    }
                }

                if let Some(ref mut whisper_state) = whisper_state {
                    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
                    params.set_language(Some("en"));
                    params.set_print_progress(false);
                    params.set_print_special(false);
                    params.set_print_realtime(false);
                    params.set_print_timestamps(false);

                    if let Err(e) = whisper_state.full(params, &audio_buffer[..]) {
                        let err_msg = format!("Whisper transcription failed: {}", e);
                        log_message(&app_state, LogType::Error(err_msg.clone()), &err_msg);
                    } else {
                        let num_segments = whisper_state.full_n_segments();
                        let mut full_text = String::new();

                        for i in 0..num_segments {
                            if let Some(segment) = whisper_state.get_segment(i) {
                                if let Ok(text) = segment.to_str() {
                                    full_text.push_str(text);
                                }
                            }
                        }

                        let user_text = full_text.trim();
                        if !user_text.is_empty() {
                            process_user_message(
                                &app_state,
                                &registry,
                                &client,
                                &mut messages,
                                max_context_messages,
                                user_text,
                            );
                        }
                    }
                } else {
                    let err_msg = "Whisper transcription is unavailable because the model failed to load.".to_string();
                    log_message(&app_state, LogType::Error(err_msg.clone()), &err_msg);
                }

                audio_buffer.clear();
                {
                    if let Ok(mut state) = app_state.lock() {
                        state.agent_state = AgentState::Idle;
                    }
                }
            }
            AiMessage::TextInput(text) => {
                let user_text = text.trim();
                if !user_text.is_empty() {
                    audio_buffer.clear(); // Flush audio queue
                    {
                        if let Ok(mut state) = app_state.lock() {
                            state.agent_state = AgentState::Thinking;
                        }
                    }

                    process_user_message(
                        &app_state,
                        &registry,
                        &client,
                        &mut messages,
                        max_context_messages,
                        user_text,
                    );

                    {
                        if let Ok(mut state) = app_state.lock() {
                            state.agent_state = AgentState::Idle;
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

fn process_user_message(
    app_state: &Arc<Mutex<AppState>>,
    registry: &SkillRegistry,
    client: &reqwest::blocking::Client,
    messages: &mut Vec<ChatMessage>,
    max_context_messages: usize,
    user_text: &str,
) {
    // 1. Log the user's transcript
    log_message(app_state, LogType::User, user_text);

    // 2. Feed to chat context
    messages.push(ChatMessage {
        role: "user".to_string(),
        content: user_text.to_string(),
    });

    let mut max_iterations = 5;
    while max_iterations > 0 {
        max_iterations -= 1;

        // Truncate context if it gets too large
        if messages.len() > max_context_messages {
            let mut truncated = vec![messages[0].clone()];
            let recent_start_idx = messages.len() - (max_context_messages - 1);
            truncated.extend_from_slice(&messages[recent_start_idx..]);
            *messages = truncated;
        }

        let (current_url, current_model) = {
            if let Ok(state) = app_state.lock() {
                (state.config.server_url.clone(), state.config.model_name.clone())
            } else {
                break;
            }
        };

        let request_body = ChatRequest {
            model: current_model,
            messages: messages.clone(),
            stream: false,
        };

        match client.post(&current_url).json(&request_body).send() {
            Ok(res) => {
                if !res.status().is_success() {
                    let err_msg = format!("Ollama server returned error status: {}", res.status());
                    log_message(app_state, LogType::Error(err_msg.clone()), &err_msg);
                    break;
                }

                if let Ok(chat_res) = res.json::<ChatResponse>() {
                    let raw_reply = chat_res.message.content.trim();
                    let cleaned_reply = clean_json_string(raw_reply);

                    // Inspect if response is a JSON Tool Call
                    if let Ok(action_call) = serde_json::from_str::<GenericActionCall>(&cleaned_reply) {
                        let tool_name = action_call.action.clone();
                        let tool_args = action_call.parameters.to_string();

                        log_message(
                            app_state,
                            LogType::ToolCall {
                                name: tool_name.clone(),
                                args: tool_args,
                            },
                            &format!("🤖 Triggering skill '{}'...", tool_name),
                        );

                        let skill_result = if let Some(skill) = registry.get(&tool_name) {
                            let current_config = {
                                if let Ok(state) = app_state.lock() {
                                    state.config.clone()
                                } else {
                                    break;
                                }
                            };
                            skill.execute(&action_call.parameters, &current_config)
                        } else {
                            Err(anyhow!("Skill '{}' not found in registry.", tool_name))
                        };

                        match skill_result {
                            Ok(result_str) => {
                                log_message(
                                    app_state,
                                    LogType::ToolResult {
                                        name: tool_name.clone(),
                                        result: result_str.clone(),
                                    },
                                    &format!("Skill '{}' completed successfully.", tool_name),
                                );

                                // Feed result back into conversation history
                                messages.push(ChatMessage {
                                    role: "assistant".to_string(),
                                    content: raw_reply.to_string(),
                                });

                                messages.push(ChatMessage {
                                    role: "user".to_string(),
                                    content: format!(
                                        "System tool observation for '{}': {}. Explain this to the user conversationally.",
                                        tool_name, result_str
                                    ),
                                });

                                // Loop back to have model create the conversational final reply
                                continue;
                            }
                            Err(e) => {
                                let err_msg = format!("Skill '{}' failed: {}", tool_name, e);
                                log_message(app_state, LogType::Error(err_msg.clone()), &err_msg);

                                messages.push(ChatMessage {
                                    role: "assistant".to_string(),
                                    content: raw_reply.to_string(),
                                });

                                messages.push(ChatMessage {
                                    role: "user".to_string(),
                                    content: format!("System tool error for '{}': {}", tool_name, e),
                                });
                                continue;
                            }
                        }
                    } else {
                        // Conversational response
                        log_message(app_state, LogType::Assistant, raw_reply);

                        {
                            if let Ok(mut state) = app_state.lock() {
                                state.agent_state = AgentState::Speaking(raw_reply.to_string());
                            }
                        }

                        // Play TTS
                        speak(raw_reply, app_state);

                        messages.push(ChatMessage {
                            role: "assistant".to_string(),
                            content: raw_reply.to_string(),
                        });
                        break;
                    }
                } else {
                    let err_msg = "Failed to parse Ollama response body.".to_string();
                    log_message(app_state, LogType::Error(err_msg.clone()), &err_msg);
                    break;
                }
            }
            Err(e) => {
                let err_msg = format!("Failed to connect to Ollama server at {}: {}", current_url, e);
                log_message(app_state, LogType::Error(err_msg.clone()), &err_msg);
                break;
            }
        }
    }
}
