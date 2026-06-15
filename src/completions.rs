//! Shell completion script generators for the `smolcode` CLI.
//!
//! Pure-std, hand-templated bash/zsh/fish completions. No clap dependency.

/// Supported shells for completion generation.
#[allow(dead_code)] // public API: used by tests + future `--completions` listing
pub fn shells() -> &'static [&'static str] {
    &["bash", "zsh", "fish"]
}

/// Return a ready-to-source completion script for `shell`, or an
/// `Err(String)` naming the valid shells if `shell` is unknown.
///
/// The match on `shell` is case-insensitive.
pub fn generate(shell: &str) -> Result<String, String> {
    match shell.to_ascii_lowercase().as_str() {
        "bash" => Ok(bash().to_string()),
        "zsh" => Ok(zsh().to_string()),
        "fish" => Ok(fish().to_string()),
        other => Err(format!(
            "unknown shell '{other}'; supported: bash, zsh, fish"
        )),
    }
}

fn bash() -> &'static str {
    r#"_smolcode() {
    local cur prev opts
    COMPREPLY=()
    cur="${COMP_WORDS[COMP_CWORD]}"
    prev="${COMP_WORDS[COMP_CWORD-1]}"
    opts="--model --url --key --agent --plan --dir --yolo --local --no-tui --repl --completions -h --help"

    case "$prev" in
        --agent)
            COMPREPLY=( $(compgen -W "build plan" -- "$cur") )
            return 0
            ;;
        --completions)
            COMPREPLY=( $(compgen -W "bash zsh fish" -- "$cur") )
            return 0
            ;;
        --dir)
            COMPREPLY=( $(compgen -d -- "$cur") )
            return 0
            ;;
        --model|--url|--key)
            COMPREPLY=( $(compgen -f -- "$cur") )
            return 0
            ;;
    esac

    if [[ "$cur" == -* ]]; then
        COMPREPLY=( $(compgen -W "$opts" -- "$cur") )
        return 0
    fi

    COMPREPLY=( $(compgen -f -- "$cur") )
    return 0
}
complete -F _smolcode smolcode
"#
}

fn zsh() -> &'static str {
    r#"#compdef smolcode

_smolcode() {
    _arguments -s \
        '--model[model name to use]:model:' \
        '--url[API base URL]:url:' \
        '--key[API key]:key:' \
        '--agent[agent mode]:agent:(build plan)' \
        '--plan[run the plan agent]' \
        '--dir[working directory]:dir:_files -/' \
        '--yolo[skip approval prompts]' \
        '--local[use local model endpoint]' \
        '--no-tui[disable the TUI]' \
        '--repl[start an interactive REPL]' \
        '--completions[print a shell completion script]:shell:(bash zsh fish)' \
        '(-h --help)'{-h,--help}'[show help]'
}

_smolcode "$@"
"#
}

fn fish() -> &'static str {
    r#"# fish completions for smolcode
complete -c smolcode -l model -r -d 'Model name to use'
complete -c smolcode -l url -r -d 'API base URL'
complete -c smolcode -l key -r -d 'API key'
complete -c smolcode -l agent -r -a "build plan" -d 'Agent mode'
complete -c smolcode -l plan -d 'Run the plan agent'
complete -c smolcode -l dir -r -d 'Working directory'
complete -c smolcode -l yolo -d 'Skip approval prompts'
complete -c smolcode -l local -d 'Use local model endpoint'
complete -c smolcode -l no-tui -d 'Disable the TUI'
complete -c smolcode -l repl -d 'Start an interactive REPL'
complete -c smolcode -l completions -r -a "bash zsh fish" -d 'Print a shell completion script'
complete -c smolcode -s h -l help -d 'Show help'
"#
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shells_lists_three() {
        assert_eq!(shells(), &["bash", "zsh", "fish"]);
    }

    #[test]
    fn bash_script_is_usable() {
        let s = generate("bash").expect("bash ok");
        assert!(s.contains("complete -F _smolcode"));
        assert!(s.contains("--yolo"));
        assert!(s.contains("--agent"));
    }

    #[test]
    fn zsh_script_is_usable() {
        let s = generate("zsh").expect("zsh ok");
        assert!(s.contains("#compdef smolcode"));
        assert!(s.contains("--yolo"));
        assert!(s.contains("--agent"));
    }

    #[test]
    fn fish_script_is_usable() {
        let s = generate("fish").expect("fish ok");
        assert!(s.contains("complete -c smolcode"));
        // fish declares long options as `-l yolo` (which completes `--yolo`)
        assert!(s.contains("-l yolo"));
        assert!(s.contains("-l agent"));
    }

    #[test]
    fn case_insensitive() {
        assert!(generate("BASH").is_ok());
        assert!(generate("Zsh").is_ok());
        assert!(generate("FiSh").is_ok());
    }

    #[test]
    fn unknown_shell_is_err() {
        let e = generate("powershell").unwrap_err();
        assert!(e.contains("unknown shell 'powershell'"));
        assert!(e.contains("bash, zsh, fish"));
    }
}
