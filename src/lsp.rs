//! LSP grounding — spawn the right language server over stdio and collect
//! compiler/type diagnostics for a single file, to feed back to the agent.
//!
//! Speaks just enough of the Language Server Protocol (JSON-RPC with
//! `Content-Length` framing) to: `initialize` -> `initialized` ->
//! `textDocument/didOpen`, then waits for the matching
//! `textDocument/publishDiagnostics` notification (or times out).
//!
//! Everything here is defensive: any failure (no server installed, spawn
//! error, parse error, timeout) yields an empty `Vec` and never panics.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// How long to wait for the server to publish diagnostics for our file. Kept short
/// so write_file stays snappy: fast servers (pyright/gopls) respond well under this,
/// and slow cold-start servers (rust-analyzer indexing a large repo) are simply
/// skipped rather than stalling the write.
const DIAG_TIMEOUT: Duration = Duration::from_millis(2000);

/// A language server choice: the binary plus its launch args and the LSP
/// `languageId` to advertise for opened documents.
struct Server {
    bin: &'static str,
    args: &'static [&'static str],
    language_id: &'static str,
}

/// Candidate servers for a file extension, in preference order.
fn servers_for_ext(ext: &str) -> Vec<Server> {
    match ext {
        "py" => vec![
            Server { bin: "pyright-langserver", args: &["--stdio"], language_id: "python" },
            Server { bin: "pylsp", args: &[], language_id: "python" },
        ],
        "rs" => vec![Server { bin: "rust-analyzer", args: &[], language_id: "rust" }],
        "ts" => vec![Server {
            bin: "typescript-language-server",
            args: &["--stdio"],
            language_id: "typescript",
        }],
        "js" => vec![Server {
            bin: "typescript-language-server",
            args: &["--stdio"],
            language_id: "javascript",
        }],
        "go" => vec![Server { bin: "gopls", args: &[], language_id: "go" }],
        _ => vec![],
    }
}

/// Lowercase file extension of a path, or empty string.
fn ext_of(path: &str) -> String {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// `true` if the binary is found on `PATH` (via `which`).
fn on_path(bin: &str) -> bool {
    Command::new("which")
        .arg(bin)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Pick the first available server for this file's extension.
fn pick_server(path: &str) -> Option<Server> {
    servers_for_ext(&ext_of(path))
        .into_iter()
        .find(|s| on_path(s.bin))
}

/// True if any supported language server binary is on PATH.
pub fn available_for(path: &str) -> bool {
    servers_for_ext(&ext_of(path))
        .iter()
        .any(|s| on_path(s.bin))
}

/// Get diagnostics for a workspace-relative file. Spawns the right language
/// server over stdio (LSP JSON-RPC with Content-Length framing), runs
/// initialize -> initialized -> textDocument/didOpen, waits up to ~6s for
/// textDocument/publishDiagnostics for this file, returns formatted lines like
/// "line:col: error: message". Returns empty Vec if no server is installed or
/// on any error (never panics).
pub fn diagnostics(root: &Path, rel_file: &str) -> Vec<String> {
    run(root, rel_file).unwrap_or_default()
}

/// Inner fallible body; the public wrapper maps any failure to an empty Vec.
fn run(root: &Path, rel_file: &str) -> Option<Vec<String>> {
    let server = pick_server(rel_file)?;

    let abs = root.join(rel_file);
    let text = std::fs::read_to_string(&abs).ok()?;
    let canon_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let canon_file = abs.canonicalize().unwrap_or(abs);
    let root_uri = path_to_uri(&canon_root)?;
    let file_uri = path_to_uri(&canon_file)?;

    let mut child = Command::new(server.bin)
        .args(server.args)
        .current_dir(&canon_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    // The whole exchange runs under a guard so the child is always killed,
    // even on early return.
    let result = exchange(&mut child, &server, &root_uri, &file_uri, &text);

    let _ = child.kill();
    let _ = child.wait();

    Some(result.unwrap_or_default())
}

/// Drive the LSP handshake and collect diagnostics for `file_uri`.
fn exchange(
    child: &mut Child,
    server: &Server,
    root_uri: &str,
    file_uri: &str,
    text: &str,
) -> Option<Vec<String>> {
    let mut stdin = child.stdin.take()?;
    let stdout = child.stdout.take()?;

    // initialize (request id 1)
    let init = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "processId": serde_json::Value::Null,
            "rootUri": root_uri,
            "capabilities": {},
        },
    });
    write_msg(&mut stdin, &init)?;

    // initialized notification
    let initialized = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "initialized",
        "params": {},
    });
    write_msg(&mut stdin, &initialized)?;

    // textDocument/didOpen
    let did_open = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didOpen",
        "params": {
            "textDocument": {
                "uri": file_uri,
                "languageId": server.language_id,
                "version": 1,
                "text": text,
            }
        },
    });
    write_msg(&mut stdin, &did_open)?;

    // Read incoming messages on a background thread so we can apply an overall
    // timeout via the channel recv (the server may stream notifications before
    // it ever publishes diagnostics for our file).
    let (tx, rx) = mpsc::channel::<serde_json::Value>();
    let reader = thread::spawn(move || {
        let mut br = BufReader::new(stdout);
        loop {
            match read_msg(&mut br) {
                Some(v) => {
                    if tx.send(v).is_err() {
                        break;
                    }
                }
                None => break,
            }
        }
    });

    let deadline = std::time::Instant::now() + DIAG_TIMEOUT;
    let mut out: Vec<String> = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        let msg = match rx.recv_timeout(remaining) {
            Ok(m) => m,
            Err(_) => break, // timeout or sender hung up
        };
        if msg.get("method").and_then(|m| m.as_str()) == Some("textDocument/publishDiagnostics") {
            let params = match msg.get("params") {
                Some(p) => p,
                None => continue,
            };
            let uri = params.get("uri").and_then(|u| u.as_str()).unwrap_or("");
            if !uri_eq(uri, file_uri) {
                continue;
            }
            if let Some(arr) = params.get("diagnostics").and_then(|d| d.as_array()) {
                out = arr.iter().filter_map(format_diagnostic).collect();
            }
            break;
        }
    }

    // Detach the reader WITHOUT joining: it is blocked in a blocking read on the
    // server's stdout, and the caller kills the child right after we return, which
    // closes stdout and lets the detached thread exit. Joining here would instead
    // block until a slow server (rust-analyzer cold-indexing a big repo) emits its
    // next message — that was the ~20s write_file hang.
    drop(rx);
    drop(reader);
    Some(out)
}

/// Format a single LSP `Diagnostic` object as "line:col: severity: message".
fn format_diagnostic(d: &serde_json::Value) -> Option<String> {
    let start = d.get("range")?.get("start")?;
    let line = start.get("line").and_then(|n| n.as_u64()).unwrap_or(0) + 1;
    let col = start.get("character").and_then(|n| n.as_u64()).unwrap_or(0) + 1;
    let sev = match d.get("severity").and_then(|n| n.as_u64()) {
        Some(1) => "error",
        Some(2) => "warning",
        Some(3) => "info",
        Some(4) => "hint",
        _ => "info",
    };
    let msg = d.get("message").and_then(|m| m.as_str()).unwrap_or("");
    Some(format!("{line}:{col}: {sev}: {}", msg.trim()))
}

/// Write a JSON-RPC message with `Content-Length` framing.
fn write_msg<W: Write>(w: &mut W, v: &serde_json::Value) -> Option<()> {
    let body = serde_json::to_vec(v).ok()?;
    w.write_all(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes())
        .ok()?;
    w.write_all(&body).ok()?;
    w.flush().ok()?;
    Some(())
}

/// Read one `Content-Length`-framed JSON-RPC message. Returns `None` on EOF or
/// a malformed frame.
fn read_msg<R: Read>(br: &mut BufReader<R>) -> Option<serde_json::Value> {
    let mut content_len: Option<usize> = None;
    // Read headers until the blank line.
    loop {
        let mut line = String::new();
        let n = br.read_line(&mut line).ok()?;
        if n == 0 {
            return None; // EOF
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break; // end of headers
        }
        if let Some(rest) = trimmed
            .to_ascii_lowercase()
            .strip_prefix("content-length:")
        {
            content_len = rest.trim().parse::<usize>().ok();
        }
    }
    let len = content_len?;
    let mut buf = vec![0u8; len];
    br.read_exact(&mut buf).ok()?;
    serde_json::from_slice(&buf).ok()
}

/// Convert an absolute filesystem path to a `file://` URI (percent-encoding the
/// path segments). Best-effort; assumes a Unix-style absolute path.
fn path_to_uri(p: &Path) -> Option<String> {
    let s = p.to_str()?;
    let mut uri = String::from("file://");
    for ch in s.chars() {
        match ch {
            // Unreserved per RFC 3986 plus path separators we keep verbatim.
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' | '/' => uri.push(ch),
            other => {
                let mut b = [0u8; 4];
                for byte in other.encode_utf8(&mut b).as_bytes() {
                    uri.push_str(&format!("%{byte:02X}"));
                }
            }
        }
    }
    Some(uri)
}

/// Compare two `file://` URIs leniently: servers sometimes echo a slightly
/// different (de/encoded, or trailing-slash) form than we sent.
fn uri_eq(a: &str, b: &str) -> bool {
    fn norm(u: &str) -> String {
        let s = u.strip_prefix("file://").unwrap_or(u);
        let s = s.trim_end_matches('/');
        // Cheap percent-decode so "%20" and " " compare equal.
        let mut out = String::with_capacity(s.len());
        let bytes = s.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%' && i + 2 < bytes.len() {
                if let Ok(byte) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                    out.push(byte as char);
                    i += 3;
                    continue;
                }
            }
            out.push(bytes[i] as char);
            i += 1;
        }
        out
    }
    norm(a) == norm(b)
}
