//! Render the smolcode agent's effective/resolved configuration as a readable
//! multi-line report for the `/config` command. Decoupled: takes plain params,
//! not the whole `App`. Pure std + `dirs`. Never panics.

use std::path::Path;

/// Borrowed snapshot of the resolved config the TUI wants to display.
pub struct ConfigView<'a> {
    pub model: &'a str,
    pub base_url: &'a str,
    pub agent: &'a str,
    pub read_only: bool,
    pub yolo: bool,
    pub root: &'a Path,
    pub perm_read: &'a str,
    pub perm_edit: &'a str,
    pub perm_shell: &'a str,
    pub hooks_count: usize,
    pub mcp_servers: &'a [String],
    pub tool_names: &'a [&'a str],
}

const WRAP: usize = 70;
const INDENT: &str = "               "; // aligns continuation under the value column

/// Render a readable, aligned multi-line report of the effective config.
pub fn render(v: &ConfigView) -> String {
    let mode = if v.read_only { "read-only" } else { "read-write" };
    let yolo = if v.yolo { "on" } else { "off" };
    let mcp = if v.mcp_servers.is_empty() {
        "(none)".to_string()
    } else {
        v.mcp_servers.join(", ")
    };

    let mut out = String::new();
    out.push_str("smolcode configuration\n");
    out.push_str(&format!("  model:       {}\n", v.model));
    out.push_str(&format!("  endpoint:    {}\n", v.base_url));
    out.push_str(&format!("  agent:       {} ({})\n", v.agent, mode));
    out.push_str(&format!("  yolo:        {}\n", yolo));
    out.push_str(&format!("  workspace:   {}\n", v.root.display()));
    out.push_str(&format!(
        "  permissions: read={} edit={} shell={}\n",
        v.perm_read, v.perm_edit, v.perm_shell
    ));
    out.push_str(&format!("  hooks:       {} configured\n", v.hooks_count));
    out.push_str(&format!("  mcp:         {}\n", mcp));

    out.push_str(&format!("  tools:       {} (", v.tool_names.len()));
    out.push_str(&wrap_tools(v.tool_names));
    out.push_str(")\n");

    out.push_str("config files (layered, later wins):\n");
    for line in config_paths(v.root) {
        out.push_str(&format!("  {}\n", line));
    }
    out
}

/// Join the tool names with ", " and soft-wrap at ~70 chars, indenting
/// continuation lines under the value column. Clipped to ~3 lines.
fn wrap_tools(tools: &[&str]) -> String {
    if tools.is_empty() {
        return "(none)".to_string();
    }
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    for (i, t) in tools.iter().enumerate() {
        let piece = if i + 1 < tools.len() {
            format!("{}, ", t)
        } else {
            t.to_string()
        };
        if !cur.is_empty() && cur.len() + piece.len() > WRAP {
            lines.push(std::mem::take(&mut cur));
        }
        cur.push_str(&piece);
    }
    if !cur.is_empty() {
        lines.push(cur);
    }

    // Clip to ~3 lines, marking the overflow.
    let clipped = lines.len() > 3;
    lines.truncate(3);
    let mut joined = lines.join(&format!("\n{}", INDENT));
    if clipped {
        joined.push_str(" ...");
    }
    joined
}

/// The config file search paths smolcode layers (global then project), as
/// display strings, each tagged "(exists)" or "(absent)".
pub fn config_paths(root: &Path) -> Vec<String> {
    let global = dirs::config_dir()
        .unwrap_or_default()
        .join("smolcode/config.toml");
    let project = root.join(".smolcode/config.toml");
    [global, project]
        .iter()
        .map(|p| {
            let tag = if p.exists() { "(exists)" } else { "(absent)" };
            format!("{} {}", p.display(), tag)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn sample<'a>(root: &'a Path, read_only: bool, yolo: bool) -> ConfigView<'a> {
        ConfigView {
            model: "granite4.1:3b",
            base_url: "http://localhost:11435/v1",
            agent: "smol",
            read_only,
            yolo,
            root,
            perm_read: "allow",
            perm_edit: "ask",
            perm_shell: "deny",
            hooks_count: 2,
            mcp_servers: &[],
            tool_names: &["read", "edit", "shell", "grep"],
        }
    }

    #[test]
    fn render_contains_core_fields() {
        let root = PathBuf::from("/tmp/ws");
        let out = render(&sample(&root, true, true));
        assert!(out.contains("model:"));
        assert!(out.contains("granite4.1:3b"));
        assert!(out.contains("permissions:"));
        assert!(out.contains("read=allow"));
        assert!(out.contains("smol"));
        assert!(out.contains("yolo:        on"));
        assert!(out.contains("read-only"));
    }

    #[test]
    fn render_read_write_and_yolo_off() {
        let root = PathBuf::from("/tmp/ws");
        let out = render(&sample(&root, false, false));
        assert!(out.contains("read-write"));
        assert!(out.contains("yolo:        off"));
    }

    #[test]
    fn config_paths_two_entries() {
        let root = PathBuf::from("/tmp/does-not-exist-smolcode");
        let paths = config_paths(&root);
        assert_eq!(paths.len(), 2);
        assert!(paths[1].contains(".smolcode/config.toml"));
        assert!(paths[1].ends_with("(absent)"));
    }

    #[test]
    fn config_paths_exists_when_present() {
        let dir = std::env::temp_dir().join(format!("smolcode-cfgtest-{}", std::process::id()));
        let cfg_dir = dir.join(".smolcode");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(cfg_dir.join("config.toml"), b"").unwrap();

        let paths = config_paths(&dir);
        assert!(paths[1].ends_with("(exists)"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
