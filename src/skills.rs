//! Skills — named, reusable instruction bundles (Claude-Code style).
//! Each skill is a directory `<name>/SKILL.md` under `~/.config/smolcode/skills/`
//! (user) or `<root>/.smolcode/skills/` (project), with optional bundled files.
//!
//! Skills are both **model-invoked** (a one-line catalog is injected into the
//! system prompt; the agent calls the `use_skill` tool to load a skill's full
//! body) and **user-invoked** (`/skill <name> [args]` expands the body).

use crate::rules::parse_frontmatter;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Skill {
    /// Skill name (frontmatter `name:` or the directory name).
    pub name: String,
    /// One-line description (frontmatter `description:`), possibly empty.
    pub description: String,
    /// The SKILL.md body (frontmatter stripped) — the instructions to follow.
    pub body: String,
    /// The skill's directory, for any bundled supporting files.
    pub dir: PathBuf,
}

/// The two skill roots, user first so project skills override by name.
fn skill_dirs(root: &Path) -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(c) = dirs::config_dir() {
        v.push(c.join("smolcode").join("skills"));
    }
    v.push(root.join(".smolcode").join("skills"));
    v
}

/// Load every `<name>/SKILL.md` skill. Project skills override user skills with
/// the same name; the result is sorted by name.
pub fn load(root: &Path) -> Vec<Skill> {
    let mut out: Vec<Skill> = Vec::new();
    for dir in skill_dirs(root) {
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                let sdir = e.path();
                if !sdir.is_dir() {
                    continue;
                }
                let manifest = sdir.join("SKILL.md");
                let Ok(raw) = std::fs::read_to_string(&manifest) else {
                    continue;
                };
                let folder = sdir.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                let (name, description, body) = parse_skill(&raw, &folder);
                // project overrides user (later wins)
                out.retain(|s| s.name != name);
                out.push(Skill { name, description, body, dir: sdir });
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Find a single skill by name (case-insensitive).
pub fn find(root: &Path, name: &str) -> Option<Skill> {
    let want = name.trim().to_lowercase();
    load(root).into_iter().find(|s| s.name.to_lowercase() == want)
}

/// One line per skill (`- <name>: <description>`) for the system-prompt catalog.
/// Empty string when there are no skills.
pub fn catalog(root: &Path) -> String {
    let skills = load(root);
    if skills.is_empty() {
        return String::new();
    }
    skills
        .iter()
        .map(|s| {
            if s.description.is_empty() {
                format!("- {}", s.name)
            } else {
                format!("- {}: {}", s.name, s.description)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Parse a SKILL.md: frontmatter `name`/`description`, body stripped. Falls back
/// to the folder name when `name:` is absent.
fn parse_skill(raw: &str, folder: &str) -> (String, String, String) {
    let (description, body) = parse_frontmatter(raw);
    // pull an optional `name:` from the same frontmatter block, if present
    let mut name = folder.to_string();
    let trimmed = raw.strip_prefix('\u{feff}').unwrap_or(raw);
    if let Some(rest) = trimmed.strip_prefix("---\n") {
        if let Some(end) = rest.find("\n---") {
            for line in rest[..end].lines() {
                if let Some(v) = line.trim().strip_prefix("name:") {
                    let v = v.trim().trim_matches('"');
                    if !v.is_empty() {
                        name = v.to_string();
                    }
                }
            }
        }
    }
    (name, description.unwrap_or_default(), body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_skill(root: &Path, folder: &str, contents: &str) {
        let d = root.join(".smolcode").join("skills").join(folder);
        let _ = std::fs::create_dir_all(&d);
        let _ = std::fs::write(d.join("SKILL.md"), contents);
    }

    // These assert on the *project* skills we write (and the project-overrides-user
    // rule), not on exact totals, so a populated real ~/.config global dir can't
    // break them.
    #[test]
    fn load_catalog_and_project_override() {
        let root = std::env::temp_dir().join("smolcode_skills_test");
        let _ = std::fs::remove_dir_all(&root);
        write_skill(&root, "greet", "---\nname: greet\ndescription: say hello\n---\nGreet the user warmly.");
        write_skill(&root, "lint", "Run the linter."); // no frontmatter -> folder name, empty desc
        let skills = load(&root);
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"greet") && names.contains(&"lint"), "names: {names:?}");

        // project greet overrides any same-named user/global skill
        let greet = skills.iter().find(|s| s.name == "greet").unwrap();
        assert_eq!(greet.description, "say hello");
        assert_eq!(greet.body, "Greet the user warmly.");
        let lint = skills.iter().find(|s| s.name == "lint").unwrap();
        assert!(lint.description.is_empty());

        let cat = catalog(&root);
        assert!(cat.contains("- greet: say hello"));
        assert!(cat.contains("- lint"));

        assert_eq!(find(&root, "GREET").unwrap().name, "greet");
        assert!(find(&root, "definitely-not-a-skill-xyz").is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn parse_skill_name_and_description_fallbacks() {
        // frontmatter name/description honored
        let (n, d, b) = parse_skill("---\nname: foo\ndescription: bar\n---\nbody text", "folder");
        assert_eq!((n.as_str(), d.as_str(), b.as_str()), ("foo", "bar", "body text"));
        // no frontmatter -> folder name, empty description, raw body
        let (n, d, b) = parse_skill("just instructions", "myfolder");
        assert_eq!((n.as_str(), d.as_str(), b.as_str()), ("myfolder", "", "just instructions"));
    }
}
