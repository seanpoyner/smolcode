//! Custom Markdown slash-commands (opencode-style).
//! `.smolcode/commands/<name>.md` (project) or ~/.config/smolcode/commands/<name>.md
//! (global). `/name args` expands the file (with `$ARGUMENTS`) into a task.

use std::path::Path;

pub struct Cmd {
    pub name: String,
    pub body: String,
}

pub fn load(root: &Path) -> Vec<Cmd> {
    let mut roots = Vec::new();
    if let Some(c) = dirs::config_dir() {
        roots.push(c.join("smolcode").join("commands"));
    }
    roots.push(root.join(".smolcode").join("commands"));

    let mut out: Vec<Cmd> = Vec::new();
    for d in roots {
        if let Ok(rd) = std::fs::read_dir(&d) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().map_or(false, |x| x == "md") {
                    if let (Some(stem), Ok(body)) = (
                        p.file_stem().map(|s| s.to_string_lossy().to_string()),
                        std::fs::read_to_string(&p),
                    ) {
                        // project overrides global (later wins)
                        out.retain(|c| c.name != stem);
                        out.push(Cmd { name: stem, body });
                    }
                }
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

pub fn expand(body: &str, args: &str) -> String {
    if body.contains("$ARGUMENTS") {
        body.replace("$ARGUMENTS", args)
    } else if args.trim().is_empty() {
        body.to_string()
    } else {
        format!("{}\n\n{}", body.trim_end(), args)
    }
}
