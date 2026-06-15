//! MCP (Model Context Protocol) client bridge.
//!
//! Connects to stdio MCP servers declared in smolcode config (TOML `[[mcp]]`),
//! discovers their tools, and exposes them to the agent as
//! [`liteforge::ToolDefinition`]s with collision-proof, prefixed names of the
//! form `mcp__<server>__<tool>`.
//!
//! The whole module is intentionally defensive: a server that fails to start,
//! fails to list its tools, or errors on a call never panics and never aborts
//! the agent. Failures degrade to an empty tool set or a textual error result.

use liteforge::mcp::{CallToolParams, McpServer, McpServerConfig, McpStdioServer, ToolResultContent};
use liteforge::{FunctionDefinition, ToolDefinition, ToolParameters};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;

/// Separator placed between the `mcp` marker, the server name and the tool
/// name in advertised tool names (e.g. `mcp__github__create_issue`).
const PREFIX: &str = "mcp__";
const SEP: &str = "__";

/// One MCP server entry, parsed from smolcode config (TOML `[[mcp]]`).
///
/// ```toml
/// [[mcp]]
/// name = "filesystem"
/// command = "npx"
/// args = ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
/// env = { LOG_LEVEL = "info" }
/// ```
#[derive(Clone, Deserialize)]
pub struct McpServerCfg {
    /// Unique server name; used as the middle segment of the tool prefix.
    pub name: String,
    /// Executable to spawn for the stdio transport.
    pub command: String,
    /// Arguments passed to `command`.
    #[serde(default)]
    pub args: Vec<String>,
    /// Extra environment variables for the spawned process.
    #[serde(default)]
    pub env: HashMap<String, String>,
}

/// A single discovered MCP tool plus its owning server name and the
/// `ToolDefinition` we advertise to the model.
struct Entry {
    /// Config name of the server that owns this tool.
    server: String,
    /// The tool's native (unprefixed) name on its server.
    tool: String,
    /// Prefixed definition advertised to the model.
    def: ToolDefinition,
}

/// Connected MCP servers plus a flattened, prefixed tool index.
///
/// Servers are kept boxed behind the [`McpServer`] trait so their stdio
/// subprocesses stay alive for the lifetime of this struct.
#[derive(Default)]
pub struct McpTools {
    /// Connected servers, keyed by config name.
    servers: HashMap<String, Box<dyn McpServer>>,
    /// One entry per discovered tool, keyed by prefixed advertised name.
    tools: HashMap<String, Entry>,
}

impl McpTools {
    /// Connect all configured stdio MCP servers and discover their tools.
    ///
    /// Never panics. Servers that fail to connect or list tools are skipped.
    /// An empty `cfgs` yields an `McpTools` with no servers and no tools.
    pub async fn connect(cfgs: Vec<McpServerCfg>) -> Self {
        let mut me = McpTools::default();
        for cfg in cfgs {
            me.connect_one(cfg).await;
        }
        me
    }

    /// Connect a single server and register its tools. Failures are swallowed.
    async fn connect_one(&mut self, cfg: McpServerCfg) {
        if cfg.name.is_empty() || cfg.command.is_empty() {
            return;
        }
        // Avoid clobbering an already-connected server with the same name.
        if self.servers.contains_key(&cfg.name) {
            return;
        }

        let mut sc = McpServerConfig::stdio(cfg.name.clone(), cfg.command.clone())
            .with_args(cfg.args.clone());
        for (k, v) in cfg.env.clone() {
            sc = sc.with_env_var(k, v);
        }

        let mut server = McpStdioServer::new(sc);
        if server.connect().await.is_err() {
            return;
        }

        // Discover tools; if listing fails we keep the connection but expose
        // nothing from it.
        let listed = match server.list_tools().await {
            Ok(r) => r.tools,
            Err(_) => Vec::new(),
        };

        for mcp_tool in listed {
            let advertised = format!("{PREFIX}{}{SEP}{}", cfg.name, mcp_tool.name);
            let def = build_def(&advertised, mcp_tool.description.as_deref(), &mcp_tool.input_schema);
            self.tools.insert(
                advertised,
                Entry {
                    server: cfg.name.clone(),
                    tool: mcp_tool.name.clone(),
                    def,
                },
            );
        }

        self.servers.insert(cfg.name.clone(), Box::new(server));
    }

    /// Tool definitions to advertise to the model.
    ///
    /// Names are prefixed (`mcp__<server>__<tool>`) to avoid collisions with
    /// the built-in tools and across servers.
    pub fn defs(&self) -> Vec<ToolDefinition> {
        self.tools.values().map(|e| e.def.clone()).collect()
    }

    /// Returns `true` if `name` is one of our advertised MCP tools.
    pub fn has(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }

    /// Names of the connected MCP servers (for the /config view).
    pub fn server_names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.servers.keys().cloned().collect();
        v.sort();
        v
    }

    /// Connected servers paired with their (native, unprefixed) tool names,
    /// sorted by server then tool. Drives the `/mcp` listing.
    pub fn list_by_server(&self) -> Vec<(String, Vec<String>)> {
        let server_names: Vec<String> = self.servers.keys().cloned().collect();
        let pairs: Vec<(String, String)> = self
            .tools
            .values()
            .map(|e| (e.server.clone(), e.tool.clone()))
            .collect();
        group_tools(&server_names, &pairs)
    }

    /// Execute an MCP tool call and return its textual result.
    ///
    /// On any failure (unknown tool, disconnected server, bad arguments, or a
    /// tool-reported error) this returns a descriptive error string rather than
    /// erroring out, so the agent loop can keep going.
    pub async fn dispatch(&self, name: &str, args_json: &str) -> String {
        let Some(entry) = self.tools.get(name) else {
            return format!("unknown MCP tool '{name}'");
        };
        let Some(server) = self.servers.get(&entry.server) else {
            return format!("MCP server '{}' is not connected", entry.server);
        };

        // Parse args into the HashMap shape MCP expects. Anything that isn't a
        // JSON object becomes "no arguments".
        let arguments: Option<HashMap<String, Value>> = serde_json::from_str::<Value>(args_json)
            .ok()
            .and_then(|v| match v {
                Value::Object(map) => Some(map.into_iter().collect()),
                _ => None,
            });

        let params = CallToolParams {
            name: entry.tool.clone(),
            arguments,
        };

        match server.call_tool(params).await {
            Ok(result) => {
                let text = flatten_content(&result.content);
                if result.is_error.unwrap_or(false) {
                    format!("MCP tool '{name}' error: {text}")
                } else {
                    text
                }
            }
            Err(e) => format!("MCP tool '{name}' call failed: {e}"),
        }
    }
}

/// Build a [`ToolDefinition`] from an MCP tool's name, description, and
/// JSON-Schema input schema. Mirrors the construction in `src/tools.rs`.
fn build_def(name: &str, description: Option<&str>, input_schema: &Value) -> ToolDefinition {
    let properties = input_schema
        .get("properties")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    let required: Option<Vec<String>> = input_schema
        .get("required")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        });

    ToolDefinition {
        tool_type: "function".into(),
        function: FunctionDefinition {
            name: name.into(),
            description: description.map(str::to_string),
            parameters: Some(ToolParameters {
                schema_type: "object".into(),
                properties,
                required,
            }),
        },
    }
}

/// Collapse MCP tool-result content into a single string for the model.
fn flatten_content(content: &[ToolResultContent]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for c in content {
        match c {
            ToolResultContent::Text { text } => parts.push(text.clone()),
            ToolResultContent::Image { mime_type, .. } => {
                parts.push(format!("[image {mime_type}]"))
            }
            ToolResultContent::Resource { resource, text } => {
                if text.is_empty() {
                    parts.push(format!("[resource {}]", resource.uri));
                } else {
                    parts.push(format!("[resource {}] {text}", resource.uri));
                }
            }
        }
    }
    if parts.is_empty() {
        "(no output)".to_string()
    } else {
        parts.join("\n")
    }
}

/// Group `(server, tool)` pairs by server, including every name in
/// `server_names` (so a connected-but-toolless server still appears), with
/// servers and tools each sorted. Pure helper behind [`McpTools::list_by_server`].
fn group_tools(server_names: &[String], pairs: &[(String, String)]) -> Vec<(String, Vec<String>)> {
    let mut map: std::collections::BTreeMap<String, Vec<String>> = std::collections::BTreeMap::new();
    for name in server_names {
        map.entry(name.clone()).or_default();
    }
    for (server, tool) in pairs {
        map.entry(server.clone()).or_default().push(tool.clone());
    }
    map.into_iter()
        .map(|(server, mut tools)| {
            tools.sort();
            (server, tools)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::group_tools;

    fn s(x: &str) -> String {
        x.to_string()
    }

    #[test]
    fn empty_yields_empty() {
        assert!(group_tools(&[], &[]).is_empty());
    }

    #[test]
    fn groups_and_sorts() {
        let servers = vec![s("gitea"), s("time")];
        let pairs = vec![
            (s("gitea"), s("list_repos")),
            (s("gitea"), s("create_issue")),
            (s("time"), s("now")),
        ];
        let out = group_tools(&servers, &pairs);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].0, "gitea");
        assert_eq!(out[0].1, vec!["create_issue", "list_repos"]); // sorted
        assert_eq!(out[1], (s("time"), vec![s("now")]));
    }

    #[test]
    fn connected_server_with_no_tools_still_listed() {
        let out = group_tools(&[s("lonely")], &[]);
        assert_eq!(out, vec![(s("lonely"), Vec::<String>::new())]);
    }
}
