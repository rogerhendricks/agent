use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use eframe::egui;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
// use std::process::{Command, Stdio};
// use std::sync::mpsc;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

// =========================================================================
// 1. DATA STRUCTURES
// =========================================================================
//

#[derive(Clone, PartialEq)]
enum AgentState {
    Idle,
    Listening,
    Thinking,
    Speaking(String),
}

enum AudioMessage {
    AudioData(Vec<f32>),
    ProcessAudio,
}

struct AppConfig {
    server_url: String,
    model_name: String,
}

#[derive(Serialize)]
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
#[serde(tag = "action")]
enum ActionCall {
    #[serde(rename = "get_time")]
    GetTime,
    #[serde(rename = "web_search")]
    WebSearch { query: String },
}

// The messages our audio thread will send to our main AI thread
// enum AudioMessage {
//     AudioData(Vec<f32>),
//     ProcessAudio,
// }

// =========================================================================
// 2. HELPER FUNCTIONS
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

fn speak(text: &str) {
    println!("🔊 Speaking: {}", text);
    let mut piper = Command::new("piper-tts")
        .arg("--model")
        .arg("./en_US-lessac-medium.onnx")
        .arg("--output_raw")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("Failed to start Piper.");

    if let Some(mut piper_stdin) = piper.stdin.take() {
        piper_stdin
            .write_all(text.as_bytes())
            .expect("Failed to write to Piper");
    }

    let piper_output = piper.wait_with_output().expect("Failed to wait on Piper");

    let mut aplay = Command::new("aplay")
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
        .spawn()
        .expect("Failed to start aplay.");

    if let Some(mut aplay_stdin) = aplay.stdin.take() {
        aplay_stdin
            .write_all(&piper_output.stdout)
            .expect("Failed to write to aplay");
    }
    aplay.wait().expect("aplay encountered an error");
}

// =========================================================================
// 3. GUI IMPLEMENTATION
// =========================================================================

struct AgentApp {
    state: AgentState,
    ui_rx: Receiver<AgentState>,
    config: Arc<Mutex<AppConfig>>,
}

impl eframe::App for AgentApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Ok(new_state) = self.ui_rx.try_recv() {
            self.state = new_state;
        }

        // NEW: Settings Panel at the top
        egui::TopBottomPanel::top("settings_panel").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.heading("⚙ Settings");
                ui.separator();

                // Lock the mutex to read/write the config safely
                if let Ok(mut cfg) = self.config.lock() {
                    ui.label("Ollama URL:");
                    ui.add(egui::TextEdit::singleline(&mut cfg.server_url).desired_width(200.0));

                    ui.label("Model:");
                    ui.add(egui::TextEdit::singleline(&mut cfg.model_name).desired_width(100.0));
                }
            });
            ui.add_space(4.0);
        });

        // The face visualization
        egui::CentralPanel::default().show(ctx, |ui| {
            let time = ui.input(|i| i.time);
            let rect = ui.available_rect_before_wrap();
            let center = rect.center();
            let painter = ui.painter();

            let eye_spacing = 110.0;
            let eye_y_offset = -30.0;
            let mouth_y_offset = 50.0;

            let mut blink_scale = if (time % 4.0) < 0.15 { 0.1 } else { 1.0 };
            let mut look_offset = egui::Vec2::ZERO;
            let mut smile_curve = 0.0;
            let mut mouth_openness = 0.0;
            let mut mouth_w = 60.0;
            let mut face_color = egui::Color32::WHITE;
            let mut text_label = "Idle";

            match &self.state {
                AgentState::Idle => {
                    smile_curve = 15.0;
                    mouth_w = 80.0 + 10.0 * (time * 1.5).sin() as f32;
                    face_color = egui::Color32::from_rgb(180, 180, 200);
                    text_label = "Idle";
                }
                AgentState::Listening => {
                    blink_scale = 1.1;
                    mouth_openness = 20.0 + 5.0 * (time * 10.0).sin() as f32;
                    mouth_w = 35.0;
                    face_color = egui::Color32::from_rgb(0, 200, 255);
                    text_label = "Listening...";
                }
                AgentState::Thinking => {
                    look_offset = egui::vec2(20.0 * (time * 4.0).sin() as f32, -10.0);
                    smile_curve = -10.0 * (time * 2.0).sin() as f32;
                    mouth_w = 50.0;
                    face_color = egui::Color32::from_rgb(255, 180, 0);
                    text_label = "Thinking...";
                }
                AgentState::Speaking(text) => {
                    let talk_wave = (time * 30.0).sin().abs() as f32 * 0.7
                        + (time * 17.0).cos().abs() as f32 * 0.3;
                    mouth_openness = 5.0 + 40.0 * talk_wave;
                    mouth_w = 70.0 + 15.0 * (time * 8.0).cos() as f32;
                    smile_curve = 10.0;
                    face_color = egui::Color32::from_rgb(50, 255, 100);
                    text_label = text;
                }
            }

            let glow_color = egui::Color32::from_rgba_unmultiplied(
                face_color.r(),
                face_color.g(),
                face_color.b(),
                15,
            );
            painter.circle_filled(center, 160.0, glow_color);

            let left_eye_center =
                center + egui::vec2(-eye_spacing / 2.0, eye_y_offset) + look_offset;
            let right_eye_center =
                center + egui::vec2(eye_spacing / 2.0, eye_y_offset) + look_offset;
            let eye_size = egui::vec2(18.0, 18.0 * blink_scale);

            painter.ellipse_filled(left_eye_center, eye_size, face_color);
            painter.ellipse_filled(right_eye_center, eye_size, face_color);

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
                    egui::Stroke::new(14.0, face_color),
                ));
            } else {
                painter.ellipse_filled(
                    mouth_center + egui::vec2(0.0, smile_curve / 2.0),
                    egui::vec2(mouth_w / 2.0, mouth_openness),
                    face_color,
                );
            }

            let text_pos = egui::pos2(center.x, center.y + 180.0);
            painter.text(
                text_pos,
                egui::Align2::CENTER_CENTER,
                text_label,
                egui::FontId::proportional(28.0),
                face_color,
            );
        });

        ctx.request_repaint();
    }
}

// =========================================================================
// 4. MAIN ENTRY & BACKGROUND AI THREAD
// =========================================================================
fn main() -> Result<(), eframe::Error> {
    let (ui_tx, ui_rx) = mpsc::channel::<AgentState>();

    // NEW: Initialize the shared configuration
    let shared_config = Arc::new(Mutex::new(AppConfig {
        server_url: "http://127.0.0.1:11434/api/chat".to_string(), // Default fallback
        model_name: "llama3".to_string(),
    }));

    // Clone the Arc pointer to pass into the background AI thread
    let ai_config = shared_config.clone();

    thread::spawn(move || {
        if let Err(e) = run_ai_pipeline(ui_tx, ai_config) {
            eprintln!("AI Pipeline crashed: {}", e);
        }
    });

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([800.0, 600.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Agent GUI",
        options,
        // Pass the remaining shared_config pointer to the GUI
        Box::new(|_cc| {
            Ok(Box::new(AgentApp {
                state: AgentState::Idle,
                ui_rx,
                config: shared_config,
            }))
        }),
    )
}

fn run_ai_pipeline(ui_tx: Sender<AgentState>, config: Arc<Mutex<AppConfig>>) -> Result<()> {
    println!("🤖 Loading Whisper Model...");
    let ctx_params = WhisperContextParameters::default();
    let mut ctx = WhisperContext::new_with_params("ggml-tiny.en.bin", ctx_params)
        .context("Failed to load Whisper model")?;
    let mut state = ctx.create_state().context("Failed to create state")?;

    let host = cpal::default_host();
    let device = host.default_input_device().context("No microphone found")?;

    let stream_config = cpal::StreamConfig {
        channels: 1,
        sample_rate: 16000,
        buffer_size: cpal::BufferSize::Default,
    };

    let (audio_tx, audio_rx) = mpsc::channel::<AudioMessage>();

    let mut is_recording = false;
    let mut silence_frames = 0;
    let silence_threshold = 16000;

    let ui_tx_audio = ui_tx.clone();

    let stream = device.build_input_stream(
        &stream_config,
        move |data: &[f32], _: &cpal::InputCallbackInfo| {
            let mut sum_squares = 0.0;
            for &sample in data {
                sum_squares += sample * sample;
            }
            let rms = (sum_squares / data.len() as f32).sqrt();

            if rms > 0.015 {
                if !is_recording {
                    let _ = ui_tx_audio.send(AgentState::Listening);
                    is_recording = true;
                }
                silence_frames = 0;
            } else if is_recording {
                silence_frames += data.len();
            }

            if is_recording {
                let _ = audio_tx.send(AudioMessage::AudioData(data.to_vec()));
            }

            if is_recording && silence_frames > silence_threshold {
                is_recording = false;
                silence_frames = 0;
                let _ = audio_tx.send(AudioMessage::ProcessAudio);
            }
        },
        |err| eprintln!("Audio stream error: {}", err),
        None,
    )?;

    stream.play()?;

    let mut audio_buffer: Vec<f32> = Vec::new();

    let system_prompt = r#"
You are a helpful voice assistant. Keep your answers brief and conversational.
You have access to tools. To use a tool, you MUST reply ONLY with a raw JSON object and no other text.
Available tools:
- To get the current time or date, output: {"action": "get_time"}
If you do not need a tool, just answer normally.
"#.trim().to_string();

    let mut messages = vec![ChatMessage {
        role: "system".to_string(),
        content: system_prompt.clone(),
    }];
    let client = reqwest::blocking::Client::new();
    let max_context_messages = 11;

    for message in audio_rx {
        match message {
            AudioMessage::AudioData(data) => {
                audio_buffer.extend(data);
            }
            AudioMessage::ProcessAudio => {
                let _ = ui_tx.send(AgentState::Thinking);

                let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
                params.set_language(Some("en"));
                params.set_print_progress(false);
                params.set_print_special(false);
                params.set_print_realtime(false);
                params.set_print_timestamps(false);

                if let Err(e) = state.full(params, &audio_buffer[..]) {
                    eprintln!("Whisper error: {}", e);
                } else {
                    let num_segments = state.full_n_segments();
                    let mut full_text = String::new();

                    for i in 0..num_segments {
                        if let Some(segment) = state.get_segment(i) {
                            if let Ok(text) = segment.to_str() {
                                full_text.push_str(text);
                            }
                        }
                    }

                    let user_text = full_text.trim();
                    if !user_text.is_empty() {
                        messages.push(ChatMessage {
                            role: "user".to_string(),
                            content: user_text.to_string(),
                        });

                        // NEW: Read the latest config right before making the network request
                        let (current_url, current_model) = {
                            let cfg = config.lock().unwrap();
                            (cfg.server_url.clone(), cfg.model_name.clone())
                        };

                        loop {
                            if messages.len() > max_context_messages {
                                let mut truncated = vec![messages[0].clone()];
                                let recent_start_idx = messages.len() - (max_context_messages - 1);
                                truncated.extend_from_slice(&messages[recent_start_idx..]);
                                messages = truncated;
                            }

                            // Use the dynamically loaded model name
                            let request_body = ChatRequest {
                                model: current_model.clone(),
                                messages: messages.clone(),
                                stream: false,
                            };

                            // Use the dynamically loaded URL
                            match client.post(&current_url).json(&request_body).send() {
                                Ok(res) => {
                                    if let Ok(chat_res) = res.json::<ChatResponse>() {
                                        let raw_reply = chat_res.message.content.trim();
                                        let cleaned_reply = clean_json_string(raw_reply);

                                        if let Ok(action_call) =
                                            serde_json::from_str::<ActionCall>(&cleaned_reply)
                                        {
                                            match action_call {
                                                ActionCall::GetTime => {
                                                    let time_str = chrono::Local::now()
                                                        .format("%A, %B %e at %l:%M %p")
                                                        .to_string();
                                                    messages.push(ChatMessage {
                                                        role: "assistant".to_string(),
                                                        content: raw_reply.to_string(),
                                                    });
                                                    messages.push(ChatMessage { role: "user".to_string(), content: format!("System tool observation: The current time is {}. Please tell me the time conversationally.", time_str) });
                                                    continue;
                                                }
                                                ActionCall::WebSearch { query: _ } => {
                                                    continue;
                                                }
                                            }
                                        } else {
                                            let _ = ui_tx
                                                .send(AgentState::Speaking(raw_reply.to_string()));
                                            speak(raw_reply);
                                            messages.push(ChatMessage {
                                                role: "assistant".to_string(),
                                                content: raw_reply.to_string(),
                                            });
                                            break;
                                        }
                                    } else {
                                        break;
                                    }
                                }
                                Err(e) => {
                                    eprintln!("Network error to {}: {}", current_url, e);
                                    break;
                                }
                            }
                        }
                    }
                }

                audio_buffer.clear();
                let _ = ui_tx.send(AgentState::Idle);
            }
        }
    }
    Ok(())
}
// fn main() -> Result<()> {
//     println!("🤖 Loading Whisper Model...");
//     let ctx_params = WhisperContextParameters::default();
//     // Ensure you downloaded ggml-tiny.en.bin to your project root!
//     let mut ctx = WhisperContext::new_with_params("ggml-tiny.en.bin", ctx_params)
//         .context("Failed to load Whisper model")?;
//     let mut state = ctx.create_state().context("Failed to create state")?;

//     let host = cpal::default_host();
//     let device = host.default_input_device().context("No microphone found")?;
//     println!("🎙️  Using mic: {}", device.name()?);

//     // We explicitly request 16kHz Mono audio because Whisper requires it.
//     // PipeWire/ALSA on Arch Linux will automatically resample the hardware input to match this.
//     let config = cpal::StreamConfig {
//         channels: 1,
//         sample_rate: 16000,
//         buffer_size: cpal::BufferSize::Default,
//     };

//     // Create a channel to send audio from the fast callback thread to the main processing thread
//     let (tx, rx) = mpsc::channel::<AudioMessage>();

//     // VAD State variables to be moved into the closure
//     let mut is_recording = false;
//     let mut silence_frames = 0;
//     // 16000 samples = 1 second of audio. We wait for 1 second of silence before processing.
//     let silence_threshold = 16000;

//     let stream = device.build_input_stream(
//         &config,
//         move |data: &[f32], _: &cpal::InputCallbackInfo| {
//             // Calculate RMS volume
//             let mut sum_squares = 0.0;
//             for &sample in data {
//                 sum_squares += sample * sample;
//             }
//             let rms = (sum_squares / data.len() as f32).sqrt();

//             let volume_threshold = 0.015; // Tweak this if your mic is too sensitive/quiet

//             if rms > volume_threshold {
//                 if !is_recording {
//                     println!("\n🗣️  Voice detected! Listening...");
//                     is_recording = true;
//                 }
//                 silence_frames = 0; // Reset silence counter because we heard noise
//             } else if is_recording {
//                 silence_frames += data.len();
//             }

//             // If we are currently in a recording state, send the audio chunk to the main thread
//             if is_recording {
//                 tx.send(AudioMessage::AudioData(data.to_vec())).unwrap();
//             }

//             // If we've been silent long enough, trigger processing
//             if is_recording && silence_frames > silence_threshold {
//                 is_recording = false;
//                 silence_frames = 0;
//                 tx.send(AudioMessage::ProcessAudio).unwrap();
//             }
//         },
//         |err| eprintln!("Audio stream error: {}", err),
//         None,
//     )?;

//     stream.play()?;
//     println!(
//         "🎧 Ready! Speak into the microphone. Pausing for 1 second will trigger transcription."
//     );

//     // --- MAIN AI THREAD ---
//     let mut audio_buffer: Vec<f32> = Vec::new();

//     // Listen to the channel forever
//     for message in rx {
//         match message {
//             AudioMessage::AudioData(data) => {
//                 audio_buffer.extend(data);
//             }
//             AudioMessage::ProcessAudio => {
//                 println!("⏳ Processing {} samples...", audio_buffer.len());

//                 let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
//                 params.set_language(Some("en"));
//                 params.set_print_progress(false);
//                 params.set_print_special(false);
//                 params.set_print_realtime(false);
//                 params.set_print_timestamps(false);

//                 if let Err(e) = state.full(params, &audio_buffer[..]) {
//                     eprintln!("Whisper error: {}", e);
//                 } else {
//                     // 1. full_n_segments() returns i32 directly now
//                     let num_segments = state.full_n_segments();
//                     let mut full_text = String::new();

//                     for i in 0..num_segments {
//                         // 2. The new v0.16.0 API uses get_segment(i)
//                         if let Some(segment) = state.get_segment(i) {
//                             // 3. Extract the UTF-8 string from the segment
//                             if let Ok(text) = segment.to_str() {
//                                 full_text.push_str(text);
//                             }
//                         }
//                     }

//                     println!("📝 You said: \"{}\"", full_text.trim());

//                     // --- NEW: SEND TO OLLAMA SERVER ---
//                     let user_text = full_text.trim();
//                     if !user_text.is_empty() {
//                         println!("🧠 Thinking (sending to server)...");

//                         // UPDATE THESE TWO LINES:
//                         let server_url = "http://10.0.0.32:11434/api/chat";
//                         let model_name = "gemma4:e4b";

//                         let request_body = ChatRequest {
//                                                 model: model_name.to_string(),
//                                                 messages: vec![
//                                                     ChatMessage {
//                                                         role: "system".to_string(),
//                                                         // A basic prompt for testing. We will add your JSON "Action Router" prompt later.
//                                                         content: "You are a helpful, conversational AI. Keep your responses brief and spoken-word friendly.".to_string(),
//                                                     },
//                                                     ChatMessage {
//                                                         role: "user".to_string(),
//                                                         content: user_text.to_string(),
//                                                     }
//                                                 ],
//                                                 stream: false, // Wait for the full response before proceeding
//                                             };

//                         let client = reqwest::blocking::Client::new();
//                         match client.post(server_url).json(&request_body).send() {
//                             Ok(res) => {
//                                 if let Ok(chat_res) = res.json::<ChatResponse>() {
//                                     println!("\n🤖 Agent: {}", chat_res.message.content);

//                                     // NEW: Trigger the voice!
//                                     speak(&chat_res.message.content);
//                                 } else {
//                                     eprintln!(
//                                         "Failed to parse response from Ollama. Did the server return an error?"
//                                     );
//                                 }
//                             }
//                             Err(e) => eprintln!("Network Error connecting to Ollama: {}", e),
//                         }
//                     }
//                     // --- END OLLAMA INTEGRATION ---
//                 }

//                 audio_buffer.clear();
//                 println!("\n🎧 Ready for next input...");
//             }
//         }
//     }

//     Ok(())
// }

// fn speak(text: &str) {
//     println!("🔊 Speaking: {}", text);

//     // 1. Start the Piper process
//     let mut piper = Command::new("piper-tts")
//         .arg("--model")
//         .arg("en_US-lessac-medium.onnx")
//         .arg("--output_raw")
//         .stdin(Stdio::piped())
//         .stdout(Stdio::piped())
//         .spawn()
//         .expect("Failed to start Piper. Is it installed and in your PATH?");

//     // 2. Feed the text into Piper's stdin
//     if let Some(mut piper_stdin) = piper.stdin.take() {
//         piper_stdin
//             .write_all(text.as_bytes())
//             .expect("Failed to write to Piper stdin");
//     }

//     // 3. Wait for Piper to finish generating the raw audio
//     let piper_output = piper.wait_with_output().expect("Failed to wait on Piper");

//     // 4. Play the raw audio using aplay (Arch Linux standard)
//     // Piper's medium models output 22050Hz, 16-bit Mono audio
//     let mut aplay = Command::new("aplay")
//         .arg("-r")
//         .arg("22050")
//         .arg("-f")
//         .arg("S16_LE")
//         .arg("-t")
//         .arg("raw")
//         .arg("-c")
//         .arg("1")
//         .stdin(Stdio::piped())
//         .stdout(Stdio::null())
//         .stderr(Stdio::null())
//         .spawn()
//         .expect("Failed to start aplay.");

//     // 5. Feed the raw audio into aplay
//     if let Some(mut aplay_stdin) = aplay.stdin.take() {
//         aplay_stdin
//             .write_all(&piper_output.stdout)
//             .expect("Failed to write to aplay");
//     }

//     aplay.wait().expect("aplay encountered an error");
// }
