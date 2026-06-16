//! Python bindings for the smolcode agent engine.

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use smolcode::agent::AgentEvent;
use smolcode::config::{Config, Flags};
use smolcode::engine::{Engine, RunOpts, ToolProfile};
use smolcode::router::Think;
use smolcode::session;
use smolcode::session_ops;
use smolcode::tools::ToolExtension;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::runtime::Runtime;
use tokio::sync::mpsc::Receiver;

struct PythonToolExtension {
    tools: Mutex<HashMap<String, PyObject>>,
}

impl ToolExtension for PythonToolExtension {
    fn try_dispatch(&self, name: &str, args: &str) -> Option<anyhow::Result<String>> {
        let callable = {
            let guard = self.tools.lock().ok()?;
            guard.get(name).cloned()
        };
        let Some(callable) = callable else {
            return None;
        };
        Some((|| {
            Python::with_gil(|py| {
                let args_dict: PyObject = if args.trim().is_empty() || args.trim() == "{}" {
                    PyDict::new_bound(py).into()
                } else {
                    let json: serde_json::Value = serde_json::from_str(args)?;
                    json_to_py(py, &json)?
                };
                let result = callable.call1(py, (args_dict,))?;
                let out = py_result_to_json(py, result)?;
                Ok(serde_json::to_string(&out)?)
            })
        })())
    }
}

fn json_to_py(py: Python<'_>, v: &serde_json::Value) -> PyResult<PyObject> {
    match v {
        serde_json::Value::Null => Ok(py.None()),
        serde_json::Value::Bool(b) => Ok(b.to_object(py)),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(i.to_object(py))
            } else if let Some(f) = n.as_f64() {
                Ok(f.to_object(py))
            } else {
                Ok(n.to_string().to_object(py))
            }
        }
        serde_json::Value::String(s) => Ok(s.to_object(py)),
        serde_json::Value::Array(arr) => {
            let list = pyo3::types::PyList::empty_bound(py);
            for item in arr {
                list.append(json_to_py(py, item)?)?;
            }
            Ok(list.into())
        }
        serde_json::Value::Object(map) => {
            let dict = PyDict::new_bound(py);
            for (k, val) in map {
                dict.set_item(k, json_to_py(py, val)?)?;
            }
            Ok(dict.into())
        }
    }
}

fn py_result_to_json(py: Python<'_>, obj: PyObject) -> PyResult<serde_json::Value> {
    if obj.is_none(py) {
        return Ok(serde_json::json!({}));
    }
    if let Ok(b) = obj.extract::<bool>(py) {
        return Ok(serde_json::json!(b));
    }
    if let Ok(i) = obj.extract::<i64>(py) {
        return Ok(serde_json::json!(i));
    }
    if let Ok(f) = obj.extract::<f64>(py) {
        return Ok(serde_json::json!(f));
    }
    if let Ok(s) = obj.extract::<String>(py) {
        return Ok(serde_json::json!(s));
    }
    if let Ok(dict) = obj.downcast_bound::<PyDict>(py) {
        let mut map = serde_json::Map::new();
        for (k, v) in dict.iter() {
            let key: String = k.extract()?;
            map.insert(key, py_to_json(py, v)?);
        }
        return Ok(serde_json::Value::Object(map));
    }
    if let Ok(list) = obj.downcast_bound::<pyo3::types::PyList>(py) {
        let mut arr = Vec::new();
        for item in list.iter() {
            arr.push(py_to_json(py, item)?);
        }
        return Ok(serde_json::Value::Array(arr));
    }
    Ok(serde_json::json!(obj.to_string()))
}

fn py_to_json(py: Python<'_>, obj: Bound<'_, PyAny>) -> PyResult<serde_json::Value> {
    py_result_to_json(py, obj.into())
}

struct SessionInner {
    engine: Engine,
    rx: Option<Receiver<AgentEvent>>,
    pending_approval: Option<tokio::sync::oneshot::Sender<bool>>,
    session_id: String,
    title: String,
    python_tools: Arc<PythonToolExtension>,
    runtime: Arc<Runtime>,
}

fn event_to_dict(py: Python<'_>, ev: AgentEvent) -> PyResult<(PyObject, Option<tokio::sync::oneshot::Sender<bool>>)> {
    let dict = PyDict::new_bound(py);
    match ev {
        AgentEvent::Token(s) => {
            dict.set_item("kind", "token")?;
            dict.set_item("text", s)?;
            Ok((dict.into(), None))
        }
        AgentEvent::Assistant(s) => {
            dict.set_item("kind", "assistant")?;
            dict.set_item("text", s)?;
            Ok((dict.into(), None))
        }
        AgentEvent::ToolCall { name, args } => {
            dict.set_item("kind", "tool_call")?;
            dict.set_item("name", name)?;
            dict.set_item("args", args)?;
            Ok((dict.into(), None))
        }
        AgentEvent::ToolResult { name, text } => {
            dict.set_item("kind", "tool_result")?;
            dict.set_item("name", name)?;
            dict.set_item("text", text)?;
            Ok((dict.into(), None))
        }
        AgentEvent::Approval { desc, resp } => {
            dict.set_item("kind", "approval")?;
            dict.set_item("desc", desc)?;
            Ok((dict.into(), Some(resp)))
        }
        AgentEvent::Final(s) => {
            dict.set_item("kind", "final")?;
            dict.set_item("text", s)?;
            Ok((dict.into(), None))
        }
        AgentEvent::Error(s) => {
            dict.set_item("kind", "error")?;
            dict.set_item("text", s)?;
            Ok((dict.into(), None))
        }
        AgentEvent::Done => {
            dict.set_item("kind", "done")?;
            Ok((dict.into(), None))
        }
    }
}

fn parse_think(s: &str) -> Think {
    match s.to_lowercase().as_str() {
        "low" => Think::Low,
        "high" => Think::High,
        "xtra" | "extra" => Think::Xtra,
        _ => Think::Off,
    }
}

fn parse_profile(s: &str) -> ToolProfile {
    match s {
        "plan" => ToolProfile::Plan,
        "web" | "web_builder" => ToolProfile::Web,
        _ => ToolProfile::Full,
    }
}

#[pyclass(name = "Config")]
struct PyConfig {
    inner: Config,
}

#[pymethods]
impl PyConfig {
    #[staticmethod]
    fn load(
        model: Option<String>,
        base_url: Option<String>,
        api_key: Option<String>,
        agent: Option<String>,
        yolo: Option<bool>,
    ) -> PyResult<Self> {
        let mut flags = Flags::default();
        flags.model = model;
        flags.base_url = base_url;
        flags.api_key = api_key;
        flags.agent = agent;
        flags.yolo = yolo.unwrap_or(false);
        Ok(Self {
            inner: Config::load(flags),
        })
    }

    #[getter]
    fn model(&self) -> String {
        self.inner.model.clone()
    }

    #[getter]
    fn base_url(&self) -> String {
        self.inner.base_url.clone()
    }

    #[getter]
    fn agent(&self) -> String {
        self.inner.agent.clone()
    }

    #[getter]
    fn yolo(&self) -> bool {
        self.inner.yolo
    }
}

#[pyclass]
struct Session {
    inner: Arc<Mutex<SessionInner>>,
}

#[pymethods]
impl Session {
    #[new]
    #[pyo3(signature = (workspace=".", agent="build", yolo=false, model=None, base_url=None, api_key=None, profile="full"))]
    fn new(
        workspace: &str,
        agent: &str,
        yolo: bool,
        model: Option<String>,
        base_url: Option<String>,
        api_key: Option<String>,
        profile: &str,
    ) -> PyResult<Self> {
        let rt = Arc::new(
            Runtime::new().map_err(|e| PyRuntimeError::new_err(format!("tokio runtime: {e}")))?,
        );
        let mut flags = Flags::default();
        flags.model = model;
        flags.base_url = base_url;
        flags.api_key = api_key;
        flags.agent = Some(agent.to_string());
        flags.yolo = yolo;
        let engine = rt
            .block_on(Engine::open_with(flags, workspace, agent, yolo))
            .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
        let mut engine = engine;
        engine.profile = parse_profile(profile);
        let python_tools = Arc::new(PythonToolExtension {
            tools: Mutex::new(HashMap::new()),
        });
        engine.tools = engine
            .tools
            .clone()
            .with_extension(python_tools.clone());
        let sid = session::new_id();
        let title = format!("session {sid}");
        Ok(Self {
            inner: Arc::new(Mutex::new(SessionInner {
                engine,
                rx: None,
                pending_approval: None,
                session_id: sid,
                title,
                python_tools,
                runtime: rt,
            })),
        })
    }

    #[getter]
    fn session_id(&self) -> PyResult<String> {
        Ok(self.inner.lock().map_err(lock_err)?.session_id.clone())
    }

    #[getter]
    fn title(&self) -> PyResult<String> {
        Ok(self.inner.lock().map_err(lock_err)?.title.clone())
    }

    fn set_title(&self, title: String) -> PyResult<()> {
        let mut g = self.inner.lock().map_err(lock_err)?;
        g.title = title;
        Ok(())
    }

    fn set_model(&self, model: String) -> PyResult<()> {
        self.inner.lock().map_err(lock_err)?.engine.set_model(model);
        Ok(())
    }

    fn set_agent(&self, agent: String) -> PyResult<()> {
        self.inner.lock().map_err(lock_err)?.engine.set_agent(&agent);
        Ok(())
    }

    fn set_think(&self, level: &str) -> PyResult<()> {
        self.inner
            .lock()
            .map_err(lock_err)?
            .engine
            .set_think(parse_think(level));
        Ok(())
    }

    fn workspace(&self) -> PyResult<String> {
        Ok(self
            .inner
            .lock()
            .map_err(lock_err)?
            .engine
            .workspace()
            .display()
            .to_string())
    }

    fn workspace_files(&self) -> PyResult<Vec<String>> {
        Ok(self
            .inner
            .lock()
            .map_err(lock_err)?
            .engine
            .workspace_files())
    }

    fn read_file(&self, path: &str) -> PyResult<Option<String>> {
        Ok(self
            .inner
            .lock()
            .map_err(lock_err)?
            .engine
            .read_workspace_file(path))
    }

    fn register_tool(&self, name: String, callable: PyObject) -> PyResult<()> {
        let g = self.inner.lock().map_err(lock_err)?;
        g.python_tools
            .tools
            .lock()
            .map_err(lock_err)?
            .insert(name, callable);
        Ok(())
    }

    fn start_turn(&self, task: String, think: Option<String>, yolo: Option<bool>) -> PyResult<()> {
        let mut g = self.inner.lock().map_err(lock_err)?;
        if g.rx.is_some() {
            return Err(PyRuntimeError::new_err("turn already in progress"));
        }
        let opts = RunOpts {
            task,
            think: think
                .map(|t| parse_think(&t))
                .unwrap_or(g.engine.think),
            yolo: yolo.unwrap_or(g.engine.cfg.yolo),
        };
        let rt = g.runtime.clone();
        let rx = {
            let engine = &mut g.engine;
            rt.block_on(engine.run_turn(opts))
        };
        g.rx = Some(rx);
        Ok(())
    }

    fn poll_event(&self, py: Python<'_>) -> PyResult<Option<PyObject>> {
        let mut g = self.inner.lock().map_err(lock_err)?;
        if g.rx.is_none() {
            return Ok(None);
        }
        let rt = g.runtime.clone();
        let ev = {
            let rx = g.rx.as_mut().unwrap();
            rt.block_on(async { rx.recv().await })
        }
        .ok_or_else(|| PyRuntimeError::new_err("event channel closed"))?;
        if matches!(ev, AgentEvent::Done) {
            g.rx = None;
        }
        let (dict, approval) = event_to_dict(py, ev)?;
        g.pending_approval = approval;
        Ok(Some(dict))
    }

    fn approve(&self, approved: bool) -> PyResult<()> {
        let mut g = self.inner.lock().map_err(lock_err)?;
        if let Some(tx) = g.pending_approval.take() {
            let _ = tx.send(approved);
        }
        Ok(())
    }

    fn run_shell(&self, command: &str) -> PyResult<String> {
        let g = self.inner.lock().map_err(lock_err)?;
        Ok(smolcode::tools::run_shell_sync(
            g.engine.workspace(),
            command,
        ))
    }

    #[staticmethod]
    fn list_sessions() -> PyResult<Vec<PyObject>> {
        Python::with_gil(|py| {
            let mut out = Vec::new();
            for meta in session::list() {
                let d = PyDict::new_bound(py);
                d.set_item("id", meta.id)?;
                d.set_item("title", meta.title)?;
                d.set_item("updated", meta.updated)?;
                out.push(d.into());
            }
            Ok(out)
        })
    }

    fn save(&self) -> PyResult<()> {
        let g = self.inner.lock().map_err(lock_err)?;
        let s = session::Session {
            id: g.session_id.clone(),
            title: g.title.clone(),
            created: session::now(),
            updated: session::now(),
            lines: vec![],
            convo: g.engine.history.clone(),
        };
        session::save(&s);
        Ok(())
    }

    fn load_session(&self, session_id: String) -> PyResult<bool> {
        let Some(s) = session::load(&session_id) else {
            return Ok(false);
        };
        let mut g = self.inner.lock().map_err(lock_err)?;
        g.session_id = s.id;
        g.title = s.title;
        g.engine.history = s.convo;
        Ok(true)
    }

    fn fork(&self) -> PyResult<Option<String>> {
        let g = self.inner.lock().map_err(lock_err)?;
        Ok(session_ops::fork(&g.session_id).map(|s| s.id))
    }

    fn rename(&self, title: String) -> PyResult<bool> {
        let g = self.inner.lock().map_err(lock_err)?;
        Ok(session_ops::rename(&g.session_id, &title))
    }

    fn delete(&self) -> PyResult<bool> {
        let g = self.inner.lock().map_err(lock_err)?;
        Ok(session_ops::delete(&g.session_id))
    }

    fn record_turn(&self, user: String, assistant: String) -> PyResult<()> {
        self.inner
            .lock()
            .map_err(lock_err)?
            .engine
            .record_turn(user, assistant);
        Ok(())
    }

    fn list_mcp(&self, py: Python<'_>) -> PyResult<Vec<PyObject>> {
        let g = self.inner.lock().map_err(lock_err)?;
        let servers = g.engine.mcp.list_by_server();
        let mut out = Vec::new();
        for (server, tools) in servers {
            let d = PyDict::new_bound(py);
            d.set_item("server", server)?;
            d.set_item("tools", tools)?;
            out.push(d.into());
        }
        Ok(out)
    }

    fn cancel_turn(&self) -> PyResult<()> {
        let mut g = self.inner.lock().map_err(lock_err)?;
        g.rx = None;
        if let Some(tx) = g.pending_approval.take() {
            let _ = tx.send(false);
        }
        Ok(())
    }

    fn render_config(&self) -> PyResult<String> {
        let g = self.inner.lock().map_err(lock_err)?;
        let engine = &g.engine;
        let ro = engine.agent.read_only;
        let yolo = engine.cfg.yolo;
        let servers = engine.mcp.server_names();
        let tool_names: Vec<&str> = smolcode::tools::tool_names();
        let view = smolcode::config_view::ConfigView {
            model: &engine.cfg.model,
            base_url: &engine.cfg.base_url,
            agent: engine.agent.name.as_str(),
            read_only: ro,
            yolo,
            root: &engine.root,
            perm_read: py_perm_label(ro, yolo, "read"),
            perm_edit: py_perm_label(ro, yolo, "edit"),
            perm_shell: py_perm_label(ro, yolo, "shell"),
            hooks_count: engine.hooks.hooks.len(),
            mcp_servers: &servers,
            tool_names: &tool_names,
        };
        Ok(smolcode::config_view::render(&view))
    }
}

fn lock_err<T: std::fmt::Display>(e: T) -> PyErr {
    PyRuntimeError::new_err(format!("lock poisoned: {e}"))
}

fn resolve_workspace(workspace: &str) -> PyResult<PathBuf> {
    std::fs::canonicalize(workspace)
        .map_err(|e| PyRuntimeError::new_err(format!("workspace dir: {e}")))
}

fn py_perm_label(read_only: bool, yolo: bool, cap: &str) -> &'static str {
    if yolo {
        return "allow";
    }
    if read_only && cap != "read" {
        return "deny";
    }
    "ask"
}

#[pyfunction]
#[pyo3(signature = (workspace))]
fn list_commands(workspace: &str) -> PyResult<Vec<String>> {
    let root = resolve_workspace(workspace)?;
    Ok(smolcode::commands::load(&root)
        .into_iter()
        .map(|c| c.name)
        .collect())
}

#[pyfunction]
#[pyo3(signature = (workspace, name, args=""))]
fn expand_command(workspace: &str, name: &str, args: &str) -> PyResult<Option<String>> {
    let root = resolve_workspace(workspace)?;
    let cmds = smolcode::commands::load(&root);
    let Some(cmd) = cmds.iter().find(|c| c.name == name) else {
        return Ok(None);
    };
    Ok(Some(smolcode::commands::expand(&cmd.body, args)))
}

#[pyfunction]
#[pyo3(signature = (session_id))]
fn get_session_chat(py: Python<'_>, session_id: &str) -> PyResult<Vec<PyObject>> {
    let Some(s) = session::load(session_id) else {
        return Ok(vec![]);
    };
    let mut out = Vec::new();
    for m in &s.lines {
        let d = PyDict::new_bound(py);
        d.set_item("role", &m.role)?;
        d.set_item("text", &m.text)?;
        out.push(d.into());
    }
    Ok(out)
}

#[pyfunction]
#[pyo3(signature = (workspace))]
fn list_rules(py: Python<'_>, workspace: &str) -> PyResult<Vec<PyObject>> {
    let root = resolve_workspace(workspace)?;
    let mut out = Vec::new();
    for r in smolcode::rules::load(&root) {
        let d = PyDict::new_bound(py);
        d.set_item("name", &r.name)?;
        d.set_item("scope", r.scope)?;
        d.set_item("description", r.description.clone().unwrap_or_default())?;
        out.push(d.into());
    }
    Ok(out)
}

#[pyfunction]
#[pyo3(signature = (workspace))]
fn list_skills(py: Python<'_>, workspace: &str) -> PyResult<Vec<PyObject>> {
    let root = resolve_workspace(workspace)?;
    let mut out = Vec::new();
    for s in smolcode::skills::load(&root) {
        let d = PyDict::new_bound(py);
        d.set_item("name", &s.name)?;
        d.set_item("description", &s.description)?;
        out.push(d.into());
    }
    Ok(out)
}

#[pyfunction]
#[pyo3(signature = (workspace, name, args=""))]
fn expand_skill(workspace: &str, name: &str, args: &str) -> PyResult<Option<String>> {
    let root = resolve_workspace(workspace)?;
    let Some(skill) = smolcode::skills::find(&root, name) else {
        return Ok(None);
    };
    Ok(Some(smolcode::commands::expand(&skill.body, args)))
}

#[pyfunction]
fn list_background_jobs() -> String {
    smolcode::bgproc::list()
}

#[pyfunction]
#[pyo3(signature = (workspace))]
fn write_agents_md(workspace: &str) -> PyResult<String> {
    let root = resolve_workspace(workspace)?;
    smolcode::agents_init::write(&root)
        .map_err(|e| PyRuntimeError::new_err(e))
}

#[pyfunction]
#[pyo3(signature = (workspace))]
fn git_status(workspace: &str) -> PyResult<String> {
    let root = resolve_workspace(workspace)?;
    Ok(smolcode::git::status(&root))
}

#[pyfunction]
#[pyo3(signature = (workspace, depth=3))]
fn workspace_tree(workspace: &str, depth: usize) -> PyResult<String> {
    let root = resolve_workspace(workspace)?;
    Ok(smolcode::tree::tree(&root, depth))
}

#[pyfunction]
#[pyo3(signature = (session_id, path=None))]
fn export_transcript(session_id: &str, path: Option<String>) -> PyResult<String> {
    let events = smolcode::trace::read(session_id);
    if events.is_empty() {
        return Err(PyRuntimeError::new_err(
            "nothing to export yet (run a task first)",
        ));
    }
    let md = smolcode::trace::to_markdown(&events);
    let out_path = path.unwrap_or_else(|| format!("smolcode-{session_id}.md"));
    std::fs::write(&out_path, &md).map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    Ok(out_path)
}

#[pyfunction]
#[pyo3(signature = (session_id))]
fn session_timeline(session_id: &str) -> PyResult<Vec<String>> {
    let Some(s) = session::load(session_id) else {
        return Ok(vec!["(no saved session)".into()]);
    };
    Ok(session_ops::timeline(&s))
}

#[pymodule]
fn smolcode_core(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyConfig>()?;
    m.add_class::<Session>()?;
    m.add_function(wrap_pyfunction!(list_commands, m)?)?;
    m.add_function(wrap_pyfunction!(expand_command, m)?)?;
    m.add_function(wrap_pyfunction!(get_session_chat, m)?)?;
    m.add_function(wrap_pyfunction!(list_rules, m)?)?;
    m.add_function(wrap_pyfunction!(list_skills, m)?)?;
    m.add_function(wrap_pyfunction!(expand_skill, m)?)?;
    m.add_function(wrap_pyfunction!(list_background_jobs, m)?)?;
    m.add_function(wrap_pyfunction!(write_agents_md, m)?)?;
    m.add_function(wrap_pyfunction!(git_status, m)?)?;
    m.add_function(wrap_pyfunction!(workspace_tree, m)?)?;
    m.add_function(wrap_pyfunction!(export_transcript, m)?)?;
    m.add_function(wrap_pyfunction!(session_timeline, m)?)?;
    m.add("ThinkOff", "off")?;
    m.add("ThinkLow", "low")?;
    m.add("ThinkHigh", "high")?;
    m.add("ThinkXtra", "xtra")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_roundtrip() {
        Python::with_gil(|py| {
            let v = serde_json::json!({"ok": true, "n": 1});
            let py_obj = json_to_py(py, &v).unwrap();
            let back = py_result_to_json(py, py_obj).unwrap();
            assert_eq!(back, v);
        });
    }
}
