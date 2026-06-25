//! Remote control API — TCP JSON-RPC server for automated experiment control.
//!
//! Exposes acquisition controls over a simple TCP socket using JSON-RPC 2.0.
//! Clients (e.g., Python scripts, MATLAB, LabVIEW) connect and send commands
//! to start/stop acquisition, start/stop recording, query status, etc.
//!
//! ## Protocol
//!
//! Newline-delimited JSON-RPC 2.0 over TCP (default port 4444).
//! Each request/response is a single JSON line terminated by `\n`.
//!
//! ## Supported methods:
//!
//! - `get_status` — Returns acquisition state, recording state, elapsed time
//! - `start_acquisition` — Start Demo or Device mode
//! - `stop_acquisition` — Stop all acquisition
//! - `start_recording` — Begin recording (optionally with output_dir)
//! - `stop_recording` — Stop recording
//! - `set_display` — Change display settings (time_window, amp_scale, mode)
//! - `get_channel_count` — Returns current channel count
//! - `ping` — Returns "pong" (connectivity check)

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;

use eframe::egui;

use crate::theme;

/// Lock a mutex, recovering from poisoning. A panicked worker thread must
/// not take down the GUI thread or stop acquisition.
pub fn lock_recover<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Validate an output directory received from a remote client before it is
/// used as a recording path. Rejects empty paths, embedded NUL bytes, and
/// parent-directory traversal components.
pub fn validate_output_dir(dir: &str) -> Result<(), String> {
    if dir.trim().is_empty() {
        return Err("output_dir is empty".to_string());
    }
    if dir.contains('\0') {
        return Err("output_dir contains a NUL byte".to_string());
    }
    let traversal = std::path::Path::new(dir)
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir));
    if traversal {
        return Err("output_dir must not contain '..' components".to_string());
    }
    Ok(())
}

/// Default port for the remote control server.
pub const DEFAULT_PORT: u16 = 4444;

/// A command received from a remote client.
#[derive(Debug, Clone)]
pub enum RemoteCommand {
    /// Start acquisition (mode: "demo" or "device")
    StartAcquisition { mode: String },
    /// Stop all acquisition
    StopAcquisition,
    /// Start recording (optional custom output dir)
    StartRecording { output_dir: Option<String> },
    /// Stop recording
    StopRecording,
    /// Request status
    GetStatus,
    /// Get channel count
    GetChannelCount,
    /// Change display mode ("sweep" or "roll")
    SetDisplayMode { mode: String },
    /// Ping (connectivity check)
    Ping,
}

/// Response to send back to the client.
#[derive(Debug, Clone)]
pub struct RemoteResponse {
    /// JSON-RPC id (from request).
    pub id: u64,
    /// Result JSON string (success) or error message.
    pub result: Result<String, String>,
}

/// Current application status (for `get_status` response).
#[derive(Debug, Clone)]
pub struct AppStatus {
    pub is_running: bool,
    pub is_recording: bool,
    pub elapsed_seconds: f64,
    pub channel_count: usize,
    pub sample_rate: f64,
    pub display_mode: String,
    pub recorded_blocks: u64,
}

/// Remote API server state.
#[derive(Debug, Clone)]
pub struct RemoteApiState {
    /// Whether the server is enabled.
    pub enabled: bool,
    /// Port number.
    pub port: u16,
    /// Whether the server is currently running.
    pub running: bool,
    /// Number of connected clients.
    pub client_count: usize,
    /// Error message (if server failed to start).
    pub error: Option<String>,
}

impl Default for RemoteApiState {
    fn default() -> Self {
        Self {
            enabled: false,
            port: DEFAULT_PORT,
            running: false,
            client_count: 0,
            error: None,
        }
    }
}

/// Shared command queue between the TCP server thread and the GUI thread.
pub type CommandQueue = Arc<Mutex<VecDeque<(u64, RemoteCommand)>>>;
/// Shared response queue for sending replies back to clients.
pub type ResponseQueue = Arc<Mutex<VecDeque<RemoteResponse>>>;

/// Maximum bytes per line from a remote client (64 KiB). Prevents a
/// malicious or buggy client from exhausting memory.
const MAX_LINE_BYTES: usize = 64 * 1024;

/// Maximum pending commands / responses in the shared queues.
const MAX_QUEUE_LEN: usize = 256;

/// Handle to the running server (holds the stop flag and thread handle).
pub struct RemoteApiHandle {
    stop_flag: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
    bound_port: u16,
    pub commands: CommandQueue,
    pub responses: ResponseQueue,
    pub client_count: Arc<Mutex<usize>>,
}

impl RemoteApiHandle {
    /// Stop the server.
    pub fn stop(&mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        // Connect to self to unblock accept()
        let _ = TcpStream::connect(format!("127.0.0.1:{}", self.bound_port));
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for RemoteApiHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Start the remote API server on the given port.
/// Returns a handle that the GUI polls for incoming commands.
pub fn start_server(port: u16) -> Result<RemoteApiHandle, String> {
    let listener = TcpListener::bind(format!("127.0.0.1:{port}"))
        .map_err(|e| format!("Failed to bind port {port}: {e}"))?;

    listener
        .set_nonblocking(false)
        .map_err(|e| format!("Failed to set blocking: {e}"))?;

    let stop_flag = Arc::new(AtomicBool::new(false));
    let commands: CommandQueue = Arc::new(Mutex::new(VecDeque::new()));
    let responses: ResponseQueue = Arc::new(Mutex::new(VecDeque::new()));
    let client_count = Arc::new(Mutex::new(0usize));

    let stop = Arc::clone(&stop_flag);
    let cmds = Arc::clone(&commands);
    let resps = Arc::clone(&responses);
    let cc = Arc::clone(&client_count);

    let thread = thread::spawn(move || {
        server_loop(listener, stop, cmds, resps, cc);
    });

    Ok(RemoteApiHandle {
        stop_flag,
        thread: Some(thread),
        bound_port: port,
        commands,
        responses,
        client_count,
    })
}

fn server_loop(
    listener: TcpListener,
    stop: Arc<AtomicBool>,
    commands: CommandQueue,
    responses: ResponseQueue,
    client_count: Arc<Mutex<usize>>,
) {
    // The listener is already in blocking mode (set in `start_server`), so
    // `incoming()` blocks until a connection arrives; the stop flag is checked
    // on each accepted/errored connection.
    for stream in listener.incoming() {
        if stop.load(Ordering::Relaxed) {
            break;
        }

        match stream {
            Ok(stream) => {
                let stop_c = Arc::clone(&stop);
                let cmds_c = Arc::clone(&commands);
                let resps_c = Arc::clone(&responses);
                let cc_c = Arc::clone(&client_count);

                *lock_recover(&cc_c) += 1;

                thread::spawn(move || {
                    handle_client(stream, stop_c, cmds_c, resps_c);
                    *lock_recover(&cc_c) -= 1;
                });
            }
            Err(_) => {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
            }
        }
    }
}

fn handle_client(
    stream: TcpStream,
    stop: Arc<AtomicBool>,
    commands: CommandQueue,
    responses: ResponseQueue,
) {
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(500)));
    let Ok(reader_stream) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(reader_stream);
    let mut writer = stream;

    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }

        let mut line = String::new();
        match reader
            .by_ref()
            .take(MAX_LINE_BYTES as u64)
            .read_line(&mut line)
        {
            Ok(0) => break, // connection closed
            Ok(_) => {
                if let Some((id, cmd)) = parse_jsonrpc_request(&line) {
                    let mut q = lock_recover(&commands);
                    if q.len() >= MAX_QUEUE_LEN {
                        q.pop_front();
                    }
                    q.push_back((id, cmd));

                    // Wait briefly for response (poll up to 100ms)
                    let mut response_sent = false;
                    for _ in 0..20 {
                        thread::sleep(std::time::Duration::from_millis(5));
                        let mut resps = lock_recover(&responses);
                        if let Some(pos) = resps.iter().position(|r| r.id == id) {
                            let resp = resps.remove(pos).unwrap();
                            let json = format_jsonrpc_response(id, &resp.result);
                            let _ = writeln!(writer, "{json}");
                            let _ = writer.flush();
                            response_sent = true;
                            break;
                        }
                    }

                    if !response_sent {
                        let json = format_jsonrpc_response(
                            id,
                            &Err("timeout waiting for response".to_string()),
                        );
                        let _ = writeln!(writer, "{json}");
                        let _ = writer.flush();
                    }
                } else {
                    // Parse error
                    let err = r#"{"jsonrpc":"2.0","error":{"code":-32700,"message":"Parse error"},"id":null}"#;
                    let _ = writeln!(writer, "{err}");
                    let _ = writer.flush();
                }
            }
            Err(ref e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                continue;
            }
            Err(_) => break,
        }
    }
}

/// Parse a JSON-RPC 2.0 request (minimal hand-parsing to avoid serde dependency).
fn parse_jsonrpc_request(line: &str) -> Option<(u64, RemoteCommand)> {
    let line = line.trim();
    if !line.starts_with('{') || !line.ends_with('}') {
        return None;
    }

    // Extract "id" field
    let id = extract_u64_field(line, "id")?;

    // Extract "method" field
    let method = extract_string_field(line, "method")?;

    let cmd = match method.as_str() {
        "ping" => RemoteCommand::Ping,
        "get_status" => RemoteCommand::GetStatus,
        "get_channel_count" => RemoteCommand::GetChannelCount,
        "start_acquisition" => {
            let mode =
                extract_string_from_params(line, "mode").unwrap_or_else(|| "demo".to_string());
            RemoteCommand::StartAcquisition { mode }
        }
        "stop_acquisition" => RemoteCommand::StopAcquisition,
        "start_recording" => {
            let output_dir = extract_string_from_params(line, "output_dir");
            RemoteCommand::StartRecording { output_dir }
        }
        "stop_recording" => RemoteCommand::StopRecording,
        "set_display_mode" => {
            let mode =
                extract_string_from_params(line, "mode").unwrap_or_else(|| "sweep".to_string());
            RemoteCommand::SetDisplayMode { mode }
        }
        _ => return None,
    };

    Some((id, cmd))
}

/// Extract a u64 field from a JSON string (minimal parser).
fn extract_u64_field(json: &str, field: &str) -> Option<u64> {
    let pattern = format!("\"{}\"", field);
    let pos = json.find(&pattern)?;
    let after_key = &json[pos + pattern.len()..];
    // Skip : and whitespace
    let after_colon = after_key.trim_start().strip_prefix(':')?;
    let num_start = after_colon.trim_start();
    // Parse number
    let end = num_start
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(num_start.len());
    num_start[..end].parse().ok()
}

/// Extract a string field value from JSON (minimal parser).
fn extract_string_field(json: &str, field: &str) -> Option<String> {
    let pattern = format!("\"{}\"", field);
    let pos = json.find(&pattern)?;
    let after_key = &json[pos + pattern.len()..];
    let after_colon = after_key.trim_start().strip_prefix(':')?;
    let trimmed = after_colon.trim_start();
    if !trimmed.starts_with('"') {
        return None;
    }
    let content = &trimmed[1..];
    let end = content.find('"')?;
    Some(content[..end].to_string())
}

/// Extract a string field from within a "params" object.
fn extract_string_from_params(json: &str, field: &str) -> Option<String> {
    // Find params object
    let params_pos = json.find("\"params\"")?;
    let params_str = &json[params_pos..];
    extract_string_field(params_str, field)
}

/// Format a JSON-RPC 2.0 response.
fn format_jsonrpc_response(id: u64, result: &Result<String, String>) -> String {
    match result {
        Ok(value) => format!(r#"{{"jsonrpc":"2.0","result":{},"id":{}}}"#, value, id),
        Err(msg) => format!(
            r#"{{"jsonrpc":"2.0","error":{{"code":-32000,"message":"{}"}},"id":{}}}"#,
            msg.replace('"', "\\\""),
            id
        ),
    }
}

/// Format an AppStatus as a JSON result value.
pub fn format_status_json(status: &AppStatus) -> String {
    format!(
        concat!(
            "{{",
            "\"is_running\":{},",
            "\"is_recording\":{},",
            "\"elapsed_seconds\":{:.2},",
            "\"channel_count\":{},",
            "\"sample_rate\":{},",
            "\"display_mode\":\"{}\",",
            "\"recorded_blocks\":{}",
            "}}"
        ),
        status.is_running,
        status.is_recording,
        status.elapsed_seconds,
        status.channel_count,
        status.sample_rate,
        status.display_mode,
        status.recorded_blocks,
    )
}

/// Draw the remote API section in the GUI sidebar.
pub fn draw_remote_api_section(ui: &mut egui::Ui, state: &mut RemoteApiState) {
    egui::CollapsingHeader::new(
        egui::RichText::new("REMOTE API")
            .size(11.0)
            .strong()
            .color(theme::TEXT_SECONDARY),
    )
    .default_open(false)
    .show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.checkbox(
                &mut state.enabled,
                egui::RichText::new("Enable TCP server").size(10.0),
            );
        });

        if !state.enabled {
            if state.running {
                state.running = false;
            }
            return;
        }

        // Port
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Port")
                    .size(10.0)
                    .color(theme::TEXT_DIM),
            );
            let mut port = state.port as i32;
            if ui
                .add(
                    egui::DragValue::new(&mut port)
                        .range(1024..=65535)
                        .speed(1.0),
                )
                .changed()
            {
                state.port = port.max(1024) as u16;
            }
        });

        // Status
        let (color, text) = if state.running {
            (
                theme::ACCENT_GREEN,
                format!(
                    "Listening on :{} ({} client{})",
                    state.port,
                    state.client_count,
                    if state.client_count == 1 { "" } else { "s" }
                ),
            )
        } else {
            (theme::TEXT_DIM, "Stopped".to_string())
        };
        ui.label(egui::RichText::new(text).size(9.0).color(color));

        if let Some(ref err) = state.error {
            ui.colored_label(theme::ACCENT_RED, err);
        }

        // Help text
        ui.add_space(2.0);
        ui.label(
            egui::RichText::new("JSON-RPC 2.0 over TCP (newline-delimited)")
                .size(9.0)
                .italics()
                .color(theme::TEXT_DIM),
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ping_request() {
        let req = r#"{"jsonrpc":"2.0","method":"ping","id":1}"#;
        let (id, cmd) = parse_jsonrpc_request(req).unwrap();
        assert_eq!(id, 1);
        assert!(matches!(cmd, RemoteCommand::Ping));
    }

    #[test]
    fn parse_start_acquisition() {
        let req =
            r#"{"jsonrpc":"2.0","method":"start_acquisition","params":{"mode":"demo"},"id":42}"#;
        let (id, cmd) = parse_jsonrpc_request(req).unwrap();
        assert_eq!(id, 42);
        if let RemoteCommand::StartAcquisition { mode } = cmd {
            assert_eq!(mode, "demo");
        } else {
            panic!("wrong command");
        }
    }

    #[test]
    fn validate_output_dir_accepts_normal_paths() {
        assert!(validate_output_dir("recordings").is_ok());
        assert!(validate_output_dir("data/session1").is_ok());
        assert!(validate_output_dir("C:\\data\\recordings").is_ok());
    }

    #[test]
    fn validate_output_dir_rejects_bad_paths() {
        assert!(validate_output_dir("").is_err());
        assert!(validate_output_dir("   ").is_err());
        assert!(validate_output_dir("data\0dir").is_err());
        assert!(validate_output_dir("../escape").is_err());
        assert!(validate_output_dir("data/../../escape").is_err());
    }

    #[test]
    fn format_response_success() {
        let json = format_jsonrpc_response(1, &Ok("\"pong\"".to_string()));
        assert!(json.contains("\"result\":\"pong\""));
        assert!(json.contains("\"id\":1"));
    }

    #[test]
    fn format_response_error() {
        let json = format_jsonrpc_response(2, &Err("not running".to_string()));
        assert!(json.contains("\"error\""));
        assert!(json.contains("not running"));
    }

    #[test]
    fn format_status() {
        let status = AppStatus {
            is_running: true,
            is_recording: false,
            elapsed_seconds: 12.5,
            channel_count: 32,
            sample_rate: 30000.0,
            display_mode: "sweep".to_string(),
            recorded_blocks: 0,
        };
        let json = format_status_json(&status);
        assert!(json.contains("\"is_running\":true"));
        assert!(json.contains("\"channel_count\":32"));
    }

    #[test]
    fn parse_invalid_request() {
        assert!(parse_jsonrpc_request("not json").is_none());
        assert!(parse_jsonrpc_request("{}").is_none());
    }
}
