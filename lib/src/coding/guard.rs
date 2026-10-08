use std::sync::LazyLock;

use regex::Regex;

/// Destructive-command blacklist: a hit is hard-blocked, no "run anyway"
/// override -- users who need to run these must do it from the terminal
/// module manually. Patterns err toward matching loosely (extra whitespace,
/// a `sudo` prefix, ...); a false negative is worse than a false positive,
/// but this isn't meant to catch every possible destructive command either
/// -- it's one layer of defense in depth, not the only one (the terminal
/// itself remains unrestricted).
static BLACKLIST: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        Regex::new(r"rm\s+(-\w*r\w*f\w*|-\w*f\w*r\w*)\s+/(\s|$)").unwrap(),
        Regex::new(r"rm\s+(-\w*r\w*f\w*|-\w*f\w*r\w*)\s+/\*").unwrap(),
        Regex::new(r"\bmkfs(\.\w+)?\b").unwrap(),
        Regex::new(r"\bdd\s+.*of=/dev/").unwrap(),
        Regex::new(r":\(\)\s*\{\s*:\s*\|\s*:\s*&\s*\}\s*;\s*:").unwrap(), // fork bomb
        Regex::new(r">\s*/etc/passwd\b").unwrap(),
        Regex::new(r">\s*/etc/shadow\b").unwrap(),
        Regex::new(r"\bshutdown\b|\breboot\b|\bhalt\b").unwrap(),
        Regex::new(r"chmod\s+-R\s+000\s+/(\s|$)").unwrap(),
    ]
});

/// Windows-specific danger blacklist: the list above is written against
/// POSIX-style commands (`rm -rf /`) and simply won't match anything on a
/// Windows Agent target -- `Remove-Item -Recurse -Force C:\`, `format`,
/// shutdown/restart, firewall rule changes need their own pattern table,
/// applied alongside (not instead of) the list above.
static WINDOWS_BLACKLIST: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        Regex::new(r#"(?i)remove-item\s+.*-recurse\b.*c:\\?['"]?(\s|$)"#).unwrap(),
        Regex::new(r"(?i)\bformat\s+[a-z]:").unwrap(),
        Regex::new(r"(?i)\bdel\s+/[sS]\s+/[qQ]\b.*\\\s*$").unwrap(),
        Regex::new(r"(?i)\brd\s+/[sS]\s+/[qQ]\b.*\\\s*$").unwrap(),
        Regex::new(r"(?i)\bvssadmin\s+delete\b").unwrap(),
        Regex::new(r"(?i)\breg\s+delete\s+hklm\b").unwrap(),
        Regex::new(r"(?i)\bbcdedit\b").unwrap(),
        Regex::new(r"(?i)\bstop-computer\b|\brestart-computer\b|\bshutdown\s+/[rs]\b").unwrap(),
        Regex::new(r"(?i)\bdiskpart\b").unwrap(),
        Regex::new(r"(?i)netsh\s+advfirewall\s+set\s+allprofiles\s+state\s+off").unwrap(),
    ]
});

/// Read-only prefix allowlist: users can opt into auto-allowing these to cut
/// down on frequent confirmation prompts. Only matches a command's first
/// word -- doesn't mean "this command is definitely safe" (`git status &&
/// rm -rf /` gets split on `&&`/`;` and checked piece by piece, see
/// `is_whitelisted`).
static READONLY_PREFIXES: &[&str] = &[
    "ls",
    "cat",
    "grep",
    "git status",
    "git log",
    "git diff",
    "pwd",
    "whoami",
    "echo",
    "head",
    "tail",
    "find",
    "which",
    "ps",
];

/// `is_windows_target` true (`CodingTarget::Agent`) additionally applies
/// `WINDOWS_BLACKLIST` -- the two tables are a union, not either/or: a
/// Windows target can still see a POSIX-style `rm -rf /` (e.g. via Git
/// Bash), and that should still be blocked.
pub fn is_blacklisted(command: &str, is_windows_target: bool) -> bool {
    BLACKLIST.iter().any(|re| re.is_match(command))
        || (is_windows_target && WINDOWS_BLACKLIST.iter().any(|re| re.is_match(command)))
}

/// Splits on `&&`/`;`/`|` and requires every resulting sub-command to hit a
/// read-only prefix -- prevents `git status && rm -rf /important` style
/// bypasses via chaining.
pub fn is_whitelisted(command: &str) -> bool {
    command
        .split(&['&', ';', '|'][..])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .all(|part| {
            READONLY_PREFIXES
                .iter()
                .any(|prefix| part == *prefix || part.starts_with(&format!("{prefix} ")))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_rm_rf_root() {
        assert!(is_blacklisted("rm -rf /", false));
        assert!(is_blacklisted("sudo rm -fr /", false));
    }

    #[test]
    fn does_not_block_normal_rm() {
        assert!(!is_blacklisted("rm -rf ./build", false));
        assert!(!is_blacklisted("rm -rf /home/user/tmp", false));
    }

    #[test]
    fn blocks_fork_bomb() {
        assert!(is_blacklisted(":(){ :|:& };:", false));
    }

    #[test]
    fn blocks_windows_danger_commands_only_for_agent_target() {
        assert!(is_blacklisted("format c:", true));
        assert!(is_blacklisted("vssadmin delete shadows /all", true));
        assert!(is_blacklisted("Restart-Computer -Force", true));
        assert!(!is_blacklisted("format c:", false));
    }

    #[test]
    fn windows_target_still_blocks_posix_blacklist() {
        assert!(is_blacklisted("rm -rf /", true));
    }

    #[test]
    fn whitelist_allows_plain_readonly() {
        assert!(is_whitelisted("git status"));
        assert!(is_whitelisted("ls -la /var/log"));
    }

    #[test]
    fn whitelist_rejects_chained_bypass() {
        assert!(!is_whitelisted("git status && rm -rf /important"));
    }
}
