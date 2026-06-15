//! Background process manager for long-running commands (dev servers, watchers).
//!
//! Instead of blocking the agent on `npm run dev` (which never exits), we spawn
//! it detached in its own process group, tee its output to a log file, wait a
//! few seconds for a readiness marker (e.g. a "Local: http://localhost:..."
//! line), then hand control back — the job keeps running and can be read or
//! stopped later. Mirrors how opencode / Claude Code handle dev servers.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Hard cap on concurrent background jobs — a backstop against a model that
/// keeps spawning servers in a loop. Dedup handles the common case; this stops
/// runaway accumulation of *distinct* commands.
const MAX_JOBS: usize = 8;

struct Job {
    id: String,
    command: String,
    root: PathBuf,
    pid: u32,
    child: Child,
    log: PathBuf,
}

fn registry() -> &'static Mutex<Vec<Job>> {
    static R: OnceLock<Mutex<Vec<Job>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(Vec::new()))
}

/// Drop jobs whose process has already exited (so the registry reflects reality
/// and dedup/cap checks aren't fooled by dead entries). Removes their log files.
fn reap(reg: &mut Vec<Job>) {
    reg.retain_mut(|j| match j.child.try_wait() {
        Ok(Some(_)) => {
            let _ = std::fs::remove_file(&j.log);
            false
        }
        _ => true,
    });
}

fn format_jobs(reg: &[Job]) -> String {
    if reg.is_empty() {
        return "(no background jobs running)".into();
    }
    let mut out = String::from("background jobs:\n");
    for j in reg.iter() {
        out.push_str(&format!("  {} (pid {}) — {}\n", j.id, j.pid, clip(&j.command, 80)));
    }
    out.trim_end().to_string()
}

fn next_id() -> String {
    static C: OnceLock<Mutex<usize>> = OnceLock::new();
    let mut c = C.get_or_init(|| Mutex::new(0)).lock().unwrap();
    *c += 1;
    format!("bg{c}")
}

/// Substrings (lowercased) that indicate a server/watcher is up and serving.
const READY_MARKERS: &[&str] = &[
    "localhost:", "127.0.0.1:", "http://", "https://", "ready in", "ready in ",
    "listening", "compiled successfully", "compiled with", "running at",
    "server running", "watching for", "waiting for changes", "started server",
    "local:", "network:", "built in", "dev server running",
];

fn read_log(path: &Path) -> String {
    let mut s = String::new();
    if let Ok(mut f) = File::open(path) {
        let _ = f.read_to_string(&mut s);
    }
    s
}

fn looks_ready(log: &str) -> bool {
    let l = log.to_lowercase();
    READY_MARKERS.iter().any(|m| l.contains(m))
}

/// Start `command` as a background job under `root`. Waits up to ~12s for a
/// readiness marker (or early exit), then returns a summary. The job keeps
/// running; read its output with [`output`] and stop it with [`stop`].
pub fn start(root: &Path, command: &str) -> String {
    // Reap dead jobs, then refuse to start a duplicate of something already
    // running in the same dir — re-running `npm start` must NOT spawn a second
    // server (dev servers like CRA climb to the next free port, leaking one per
    // attempt). Also enforce a hard cap as a runaway backstop.
    {
        let mut reg = registry().lock().unwrap();
        reap(&mut reg);
        if let Some(j) = reg.iter().find(|j| j.command == command && j.root.as_path() == root) {
            let tail = clip(read_log(&j.log).trim(), 1500);
            return format!(
                "`{}` is ALREADY running in the background as job {} (pid {}) — not starting another. \
                 Do not start it again; reuse it. Read its output with bash_output(id=\"{}\") or stop it \
                 with stop_shell(id=\"{}\") before restarting.\n--- current output ---\n{tail}",
                clip(command, 80), j.id, j.pid, j.id, j.id,
            );
        }
        if reg.len() >= MAX_JOBS {
            return format!(
                "too many background jobs already running ({}/{MAX_JOBS}). Stop some with \
                 stop_shell(id=...) before starting more:\n{}",
                reg.len(), format_jobs(&reg),
            );
        }
    }

    let id = next_id();
    let log = std::env::temp_dir().join(format!("smolcode-{}-{}.log", std::process::id(), id));
    let out = match File::create(&log) {
        Ok(f) => f,
        Err(e) => return format!("error: could not create background log: {e}"),
    };
    let err = match out.try_clone() {
        Ok(f) => f,
        Err(e) => return format!("error: {e}"),
    };

    // Force line buffering when `stdbuf` is available so the URL line shows up
    // promptly in the log (node block-buffers when stdout is not a tty).
    // Non-login shell (`-c`, not `-lc`): smolcode already inherits the launching
    // env's PATH, and a login shell re-sources nvm, which aborts on a
    // NPM_CONFIG_PREFIX / ~/.npmrc prefix conflict and drops node from PATH —
    // breaking `npm start` / `vite` etc. See tools.rs.
    let mut cmd = if which("stdbuf") {
        let mut c = Command::new("stdbuf");
        c.arg("-oL").arg("-eL").arg("bash").arg("-c").arg(command);
        c
    } else {
        let mut c = Command::new("bash");
        c.arg("-c").arg(command);
        c
    };
    cmd.current_dir(root)
        .env_remove("NPM_CONFIG_PREFIX")
        .stdin(Stdio::null())
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(err));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0); // own process group so we can kill the whole tree
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return format!("error: failed to start background command: {e}"),
    };
    let pid = child.id();

    let deadline = Instant::now() + Duration::from_secs(12);
    let mut ready = false;
    let mut exit_code: Option<i32> = None;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                exit_code = Some(status.code().unwrap_or(-1));
                break;
            }
            Ok(None) => {}
            Err(_) => break,
        }
        if looks_ready(&read_log(&log)) {
            ready = true;
            break;
        }
        if Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    let early = clip(read_log(&log).trim(), 2500);
    if let Some(code) = exit_code {
        // Ran to completion (or crashed) quickly — treat like a normal command.
        let _ = std::fs::remove_file(&log);
        return format!("(command finished, exit {code})\n{early}");
    }

    let state = if ready { "and reported ready" } else { "and is still starting up" };
    registry().lock().unwrap().push(Job {
        id: id.clone(),
        command: command.to_string(),
        root: root.to_path_buf(),
        pid,
        child,
        log,
    });
    format!(
        "started `{}` in the background as job {id} (pid {pid}) {state}. It keeps running; \
         read its latest output with bash_output(id=\"{id}\") or stop it with stop_shell(id=\"{id}\").\n\
         --- early output ---\n{early}",
        clip(command, 80)
    )
}

/// Read the accumulated output of a background job.
pub fn output(id: &str) -> String {
    let reg = registry().lock().unwrap();
    match reg.iter().find(|j| j.id == id) {
        Some(j) => {
            let log = clip(read_log(&j.log).trim(), 6000);
            format!("output of {id} (`{}`):\n{log}", clip(&j.command, 80))
        }
        None => format!("no background job '{id}' (it may have already stopped). Use list to see active jobs."),
    }
}

/// Stop a background job (kills its whole process group).
pub fn stop(id: &str) -> String {
    let mut reg = registry().lock().unwrap();
    if let Some(pos) = reg.iter().position(|j| j.id == id) {
        let mut job = reg.remove(pos);
        kill_group(job.pid);
        let _ = job.child.kill();
        let _ = job.child.wait();
        let _ = std::fs::remove_file(&job.log);
        format!("stopped background job {id} (`{}`)", clip(&job.command, 80))
    } else {
        format!("no background job '{id}'")
    }
}

/// A one-line listing of active background jobs (reaps dead ones first).
pub fn list() -> String {
    let mut reg = registry().lock().unwrap();
    reap(&mut reg);
    format_jobs(&reg)
}

/// Kill every background job (called on exit so we don't leak dev servers).
pub fn stop_all() {
    let mut reg = registry().lock().unwrap();
    for mut job in reg.drain(..) {
        kill_group(job.pid);
        let _ = job.child.kill();
        let _ = job.child.wait();
        let _ = std::fs::remove_file(&job.log);
    }
}

/// Terminate a background job's whole process tree — SAFELY.
///
/// We never use process-group signals (`kill -<pgid>` / negative pids). A group
/// kill can, under a detach race or failure, land on OUR OWN group and take down
/// smolcode, its terminal, and the desktop session. Instead we walk `/proc` to
/// collect the job's descendants and signal each one individually by its positive
/// pid — which can only ever affect those exact processes, never a group or the
/// session. We also refuse to touch pids <= 1.
fn kill_group(pid: u32) {
    if pid <= 1 {
        return;
    }
    // Collect the target and all its descendants BEFORE signalling, so reparenting
    // during the kill can't let a child escape.
    let mut victims = vec![pid];
    collect_descendants(pid, &mut victims);
    victims.retain(|&p| p > 1);

    for &p in &victims {
        let _ = Command::new("kill").arg("-TERM").arg(p.to_string()).status();
    }
    std::thread::sleep(Duration::from_millis(250));
    for &p in &victims {
        let _ = Command::new("kill").arg("-KILL").arg(p.to_string()).status();
    }
}

/// Append every descendant pid of `parent` to `out` (depth-first), reading ppids
/// from `/proc/<pid>/stat`. Positive pids only; never a process group.
fn collect_descendants(parent: u32, out: &mut Vec<u32>) {
    let Ok(rd) = std::fs::read_dir("/proc") else { return };
    for e in rd.flatten() {
        let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        if proc_ppid(pid) == Some(parent) && !out.contains(&pid) {
            out.push(pid);
            collect_descendants(pid, out);
        }
    }
}

/// Read a process's parent pid (ppid) from `/proc/<pid>/stat`. The `comm` field
/// can contain spaces/parens, so we parse after the final ')'. None if gone.
fn proc_ppid(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_comm = &stat[stat.rfind(')')? + 1..];
    // fields after comm: state(0), ppid(1), pgrp(2), ...
    after_comm.split_whitespace().nth(1)?.parse().ok()
}

fn which(prog: &str) -> bool {
    std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .any(|d| Path::new(d).join(prog).is_file())
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max).collect();
        format!("{t}\n…(truncated)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Spawns + kills real processes, so it's excluded from the default
    // `cargo test` (which runs many in parallel). Run deliberately with:
    //   cargo test --bin smolcode -- --ignored bgproc
    #[test]
    #[ignore = "spawns/kills real processes; run explicitly"]
    fn refuses_to_start_a_duplicate() {
        let root = std::env::temp_dir();
        // prints a readiness marker so start() returns promptly, then stays alive
        let cmd = "echo localhost:65535 ; sleep 5";
        let first = start(&root, cmd);
        assert!(first.contains("background as job"), "first start: {first}");
        let second = start(&root, cmd);
        assert!(
            second.contains("ALREADY running") && second.contains("not starting another"),
            "second start should dedup: {second}"
        );
        stop_all();
        assert!(list().contains("no background jobs"), "registry should be empty after stop_all");
    }
}
