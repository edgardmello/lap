/*
 * Local embedding server managed by Lap: a Python script, such as
 * scripts/pplx_embed_server.py, that Lap starts as a child process, watches
 * until it answers /health, and stops when Lap quits.
 *
 * Lap does not install Python or the script's packages; the configured
 * interpreter (usually a venv) must already have them.
 */
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const LOG_LINES: usize = 200;
const STATUS_LOG_LINES: usize = 40;
/// Loading a vision-language model on first start can take minutes.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalServerSettings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_python")]
    pub python: String,
    #[serde(default = "default_script")]
    pub script: String,
    #[serde(default = "default_port")]
    pub port: u16,
}

fn default_python() -> String {
    "~/.venvs/pplx-embed/bin/python".to_string()
}

fn default_script() -> String {
    "~/Developer/lap/scripts/pplx_embed_server.py".to_string()
}

fn default_port() -> u16 {
    8090
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ServerState {
    Stopped,
    Starting,
    Running,
    /// Something Lap did not start already answers on the port.
    External,
    Error,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalServerStatus {
    pub state: ServerState,
    pub pid: Option<u32>,
    pub url: String,
    pub error: Option<String>,
    pub log: Vec<String>,
}

struct Managed {
    child: Option<Child>,
    settings: Option<LocalServerSettings>,
    state: ServerState,
    error: Option<String>,
    log: VecDeque<String>,
    /// Bumped on every start and stop, so watchers of an older process quit.
    generation: u64,
}

fn managed() -> &'static Mutex<Managed> {
    static MANAGED: OnceLock<Mutex<Managed>> = OnceLock::new();
    MANAGED.get_or_init(|| {
        Mutex::new(Managed {
            child: None,
            settings: None,
            state: ServerState::Stopped,
            error: None,
            log: VecDeque::new(),
            generation: 0,
        })
    })
}

fn lock() -> std::sync::MutexGuard<'static, Managed> {
    managed().lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn push_log(managed: &mut Managed, line: String) {
    if managed.log.len() >= LOG_LINES {
        managed.log.pop_front();
    }
    managed.log.push_back(line);
}

/// `~/x` becomes `$HOME/x`; anything else is used as given.
pub fn expand_home(path: &str) -> PathBuf {
    let path = path.trim();
    if path == "~" || path.starts_with("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(path.trim_start_matches('~').trim_start_matches('/'));
        }
    }
    PathBuf::from(path)
}

fn base_url(port: u16) -> String {
    format!("http://127.0.0.1:{}/v1", port)
}

fn port_answers(port: u16) -> bool {
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    TcpStream::connect_timeout(&address, Duration::from_millis(300)).is_ok()
}

fn health_ok(port: u16) -> bool {
    let Ok(client) = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()
    else {
        return false;
    };
    client
        .get(format!("http://127.0.0.1:{}/health", port))
        .send()
        .is_ok_and(|response| response.status().is_success())
}

fn status_of(managed: &Managed, port: u16) -> LocalServerStatus {
    let skip = managed.log.len().saturating_sub(STATUS_LOG_LINES);
    LocalServerStatus {
        state: managed.state,
        pid: managed.child.as_ref().map(|child| child.id()),
        url: base_url(port),
        error: managed.error.clone(),
        log: managed.log.iter().skip(skip).cloned().collect(),
    }
}

fn configured_port(managed: &Managed) -> u16 {
    managed.settings.as_ref().map_or(default_port(), |s| s.port)
}

pub fn status() -> LocalServerStatus {
    let mut managed = lock();
    let port = configured_port(&managed);
    // A server found running outside Lap may since have been stopped.
    if managed.state == ServerState::External && !port_answers(port) {
        managed.state = ServerState::Stopped;
    }
    status_of(&managed, port)
}

fn kill_child(managed: &mut Managed) {
    managed.generation += 1;
    if let Some(mut child) = managed.child.take() {
        let _ = child.kill();
        let _ = child.wait();
        push_log(managed, "Stopped by Lap".to_string());
    }
}

pub fn stop() -> LocalServerStatus {
    let mut managed = lock();
    kill_child(&mut managed);
    managed.state = ServerState::Stopped;
    managed.error = None;
    let port = configured_port(&managed);
    status_of(&managed, port)
}

/// Start the server, or keep the one already running with these settings.
pub fn start(settings: LocalServerSettings) -> LocalServerStatus {
    let mut managed = lock();
    let running = managed.child.is_some()
        && matches!(managed.state, ServerState::Starting | ServerState::Running);
    if running && managed.settings.as_ref() == Some(&settings) {
        return status_of(&managed, settings.port);
    }
    kill_child(&mut managed);
    managed.settings = Some(settings.clone());
    managed.error = None;

    if port_answers(settings.port) {
        managed.state = ServerState::External;
        return status_of(&managed, settings.port);
    }

    let python = expand_home(&settings.python);
    let script = expand_home(&settings.script);
    for (path, what) in [(&python, "Python interpreter"), (&script, "Server script")] {
        if !path.is_file() {
            managed.state = ServerState::Error;
            managed.error = Some(format!("{} not found: {}", what, path.display()));
            return status_of(&managed, settings.port);
        }
    }

    let mut command = Command::new(&python);
    command
        .arg("-u")
        .arg(&script)
        .args(["--host", "127.0.0.1", "--port", &settings.port.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(dir) = script.parent() {
        command.current_dir(dir);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            managed.state = ServerState::Error;
            managed.error = Some(format!("Could not start {}: {}", python.display(), error));
            return status_of(&managed, settings.port);
        }
    };

    managed.log.clear();
    push_log(&mut managed, format!("Starting {} {}", python.display(), script.display()));
    if let Some(stdout) = child.stdout.take() {
        std::thread::spawn(move || forward_output(stdout));
    }
    if let Some(stderr) = child.stderr.take() {
        std::thread::spawn(move || forward_output(stderr));
    }
    managed.child = Some(child);
    managed.state = ServerState::Starting;
    let generation = managed.generation;
    let port = settings.port;
    std::thread::spawn(move || watch(generation, port));
    status_of(&managed, port)
}

fn forward_output(stream: impl std::io::Read) {
    for line in BufReader::new(stream).lines().map_while(Result::ok) {
        push_log(&mut lock(), line);
    }
}

/// Follow one started process: mark it running once /health answers, and
/// record an error when it exits or never comes up.
fn watch(generation: u64, port: u16) {
    let started = Instant::now();
    loop {
        std::thread::sleep(Duration::from_millis(1000));
        let state = {
            let mut managed = lock();
            if managed.generation != generation {
                return;
            }
            let exited = managed.child.as_mut().and_then(|child| child.try_wait().ok().flatten());
            if let Some(exit) = exited {
                managed.child = None;
                managed.state = ServerState::Error;
                let last = managed.log.back().cloned().unwrap_or_default();
                managed.error = Some(format!("The server exited ({}). {}", exit, last).trim().to_string());
                return;
            }
            managed.state
        };
        if state == ServerState::Starting {
            if health_ok(port) {
                let mut managed = lock();
                if managed.generation == generation {
                    managed.state = ServerState::Running;
                    push_log(&mut managed, format!("Ready at {}", base_url(port)));
                }
            } else if started.elapsed() > STARTUP_TIMEOUT {
                let mut managed = lock();
                if managed.generation == generation {
                    kill_child(&mut managed);
                    managed.state = ServerState::Error;
                    managed.error = Some(format!(
                        "The server did not answer within {} minutes",
                        STARTUP_TIMEOUT.as_secs() / 60
                    ));
                }
                return;
            }
        }
    }
}

/// Called when Lap quits, so the server does not outlive it.
pub fn shutdown() {
    kill_child(&mut lock());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(python: &str, script: &str, port: u16) -> LocalServerSettings {
        LocalServerSettings { enabled: true, python: python.into(), script: script.into(), port }
    }

    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
    }

    fn wait_for(state: ServerState) -> LocalServerStatus {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let current = status();
            if current.state == state || Instant::now() > deadline {
                return current;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    #[test]
    fn expand_home_only_touches_a_leading_tilde() {
        let home = std::env::var("HOME").unwrap();
        assert_eq!(expand_home("~/x/y.py"), PathBuf::from(&home).join("x/y.py"));
        assert_eq!(expand_home("/opt/~/y"), PathBuf::from("/opt/~/y"));
    }

    // One test drives the shared process slot, so steps cannot interleave.
    #[test]
    fn starts_reports_running_and_stops_the_server() {
        let python = ["/usr/bin/python3", "/usr/local/bin/python3"]
            .into_iter()
            .find(|p| std::path::Path::new(p).is_file());
        let Some(python) = python else { return };
        let dir = std::env::temp_dir().join(format!("lap-local-server-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("server.py");
        std::fs::write(
            &script,
            r#"
import argparse, http.server
p = argparse.ArgumentParser(); p.add_argument("--host"); p.add_argument("--port", type=int)
a = p.parse_args()
class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200); self.end_headers(); self.wfile.write(b"{}")
print("listening", a.port, flush=True)
http.server.HTTPServer((a.host, a.port), H).serve_forever()
"#,
        )
        .unwrap();

        let missing = start(settings(python, "/no/such/script.py", free_port()));
        assert_eq!(missing.state, ServerState::Error);
        assert!(missing.error.unwrap().contains("Server script not found"));

        let port = free_port();
        let config = settings(python, script.to_str().unwrap(), port);
        assert_eq!(start(config.clone()).state, ServerState::Starting);
        let running = wait_for(ServerState::Running);
        assert_eq!(running.state, ServerState::Running, "{:?}", running.log);
        assert!(running.log.iter().any(|line| line.contains("listening")));
        let pid = running.pid;
        assert_eq!(start(config.clone()).pid, pid, "same settings keep the running process");

        assert_eq!(stop().state, ServerState::Stopped);
        assert!(!port_answers(port), "the process is gone after stop");

        // A port someone else serves is reported, not fought over.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let taken = listener.local_addr().unwrap().port();
        assert_eq!(start(settings(python, script.to_str().unwrap(), taken)).state, ServerState::External);
        let _ = std::fs::remove_dir_all(dir);
    }
}
