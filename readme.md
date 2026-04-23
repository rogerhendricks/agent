Be More Agent (Rust Edition)A fully local, highly concurrent, offline-first AI agent built in Rust. It listens via Whisper STT, reasons using a local Ollama model with function-calling capabilities, and speaks back via Piper TTS, all while driving a procedural, 60fps responsive GUI.Originally prototyped in Python, this Rust rewrite achieves dramatically lower latency, better memory safety, and eliminates the audio buffer overflows common on Linux desktop environments.🛠️ Prerequisites & DependenciesBefore compiling the Rust application, you need to ensure your Linux machine has the required system packages, audio drivers, and the necessary AI models downloaded.1. System Packages (Arch Linux)You need to install the Rust toolchain, standard build tools, and the ALSA audio utilities (for playing Piper's output).# Install Rust (if you haven't already)
curl --proto '=https' --tlsv1.2 -sSf [https://sh.rustup.rs](https://sh.rustup.rs) | sh

# Install required system dependencies
sudo pacman -S base-devel alsa-utils
2. External AI BinariesOllama (The Brain)You must have an Ollama server running. This can be on the same machine or a separate server on your local network.Install Ollama: curl -fsSL https://ollama.com/install.sh | shPull the model you intend to use: ollama pull llama3(Optional) If running Ollama on a different server, ensure OLLAMA_HOST=0.0.0.0 is set in the server's environment variables so it accepts connections.Piper TTS (The Voice)We use a standalone binary for Piper to avoid package conflicts (like the GTK gaming mouse tool also named piper).Download the pre-compiled binary:wget [https://github.com/rhasspy/piper/releases/download/v1.2.0/piper_linux_x86_64.tar.gz](https://github.com/rhasspy/piper/releases/download/v1.2.0/piper_linux_x86_64.tar.gz)
tar -xzf piper_linux_x86_64.tar.gz
Move the piper-tts executable to your system path (or ensure it is accessible as piper-tts from your project root). Alternatively, use the AUR: yay -S piper-tts-bin.3. Local Model FilesThe agent requires two specific model files placed in the root directory of your Rust project.Whisper STT Model (ggml-tiny.en.bin)Download the optimized C++ Whisper model for fast, local transcription.wget [https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.en.bin](https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.en.bin)
Piper Voice Model (.onnx & .json)Download a Lessac medium voice model (or any other English Piper model). Crucially, the .json file must exactly match the name of the .onnx file.wget [https://huggingface.co/rhasspy/piper-voices/resolve/main/en/en_US/lessac/medium/en_US-lessac-medium.onnx](https://huggingface.co/rhasspy/piper-voices/resolve/main/en/en_US/lessac/medium/en_US-lessac-medium.onnx)
wget [https://huggingface.co/rhasspy/piper-voices/resolve/main/en/en_US/lessac/medium/en_US-lessac-medium.onnx.json](https://huggingface.co/rhasspy/piper-voices/resolve/main/en/en_US/lessac/medium/en_US-lessac-medium.onnx.json)
🚀 Building & RunningOnce the dependencies and models are in place, setup is entirely handled by cargo.Clone or create the project:Navigate to the directory containing your Cargo.toml and src/main.rs.Run the Application:cargo run --release
(Note: The --release flag is highly recommended. Rust's audio processing and the Whisper-RS engine perform significantly faster in release mode compared to standard debug builds.)⚙️ Configuration & UsageWhen the application launches, an egui window will appear displaying the procedural AI core.The Settings PanelAt the top of the interface, you will find live configuration fields:Ollama URL: The REST API endpoint for your Ollama instance (e.g., http://127.0.0.1:11434/api/chat).Model: The name of the LLM you want to use (e.g., llama3).Voice: The local path to your downloaded Piper ONNX model (e.g., ./en_US-lessac-medium.onnx).Changes made in these text boxes are applied instantly to the very next conversation turn.How to InteractSpeak: Ensure your default system microphone is active. The AI core will automatically detect when you start speaking and switch to the Listening state (Cyan).Pause: Stop talking for roughly 1 second. The agent will detect the silence, process the audio buffer locally using Whisper, and switch to the Thinking state (Gold).Listen: The agent will execute any necessary system tools (like checking the time), stream the response through Piper, and switch to the Speaking state (Green, with lip-sync animations).🧩 Adding New Tools (The Action Router)The LLM is configured to use tools by outputting JSON. To add a new capability (e.g., controlling smart lights):Update the Enum (src/main.rs): Add your new tool to the ActionCall enum.#[derive(Deserialize, Debug)]
#[serde(tag = "action")]
enum ActionCall {
    #[serde(rename = "get_time")]
    GetTime,
    #[serde(rename = "toggle_lights")] // <-- NEW
    ToggleLights { room: String },     // <-- NEW
}
Update the System Prompt: Tell the LLM how to trigger the tool.- To toggle smart lights, output: {"action": "toggle_lights", "room": "living room"}
Update the Router Match Arm: Add the execution logic in the run_ai_pipeline match statement.ActionCall::ToggleLights { room } => {
    println!("Turning off lights in {}", room);
    // Add your smart home API call here

    // Feed the observation back to the LLM
    messages.push(ChatMessage { role: "assistant".to_string(), content: raw_reply.to_string() });
    messages.push(ChatMessage { role: "user".to_string(), content: format!("System observation: Turned off lights in {}", room) });
    continue;
}
