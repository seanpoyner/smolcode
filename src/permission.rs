//! opencode-style per-capability permissions: allow / ask / deny.

#[derive(Clone, Copy, PartialEq)]
pub enum Permission {
    Allow,
    Ask,
    Deny,
}

#[derive(Clone)]
pub struct PermissionSet {
    pub read: Permission,
    pub edit: Permission,
    pub shell: Permission,
}

impl PermissionSet {
    /// Defaults: reads allowed; plan denies edits/shell; yolo allows all;
    /// otherwise edits/shell ask.
    pub fn for_agent(read_only: bool, yolo: bool) -> Self {
        if read_only {
            PermissionSet { read: Permission::Allow, edit: Permission::Deny, shell: Permission::Deny }
        } else if yolo {
            PermissionSet { read: Permission::Allow, edit: Permission::Allow, shell: Permission::Allow }
        } else {
            PermissionSet { read: Permission::Allow, edit: Permission::Ask, shell: Permission::Ask }
        }
    }

    pub fn for_tool(&self, name: &str) -> Permission {
        match name {
            "read_file" | "list_dir" | "search" | "repo_map"
            | "git_status" | "git_diff" | "git_log"
            | "outline" | "find_symbol" | "find_context" | "tree" | "project_info"
            | "bash_output" | "use_skill" => self.read,
            "write_file" | "str_replace" | "apply_patch" | "multi_edit"
            | "git_commit" | "format_file" => self.edit,
            "run_shell" | "run_python" | "web_fetch" | "run_tests" | "stop_shell" => self.shell,
            // Unknown (often hallucinated) tool names: don't prompt the user to
            // approve a non-existent tool — let dispatch return the corrective
            // "not a tool" error so the model self-corrects.
            _ => Permission::Allow,
        }
    }
}
