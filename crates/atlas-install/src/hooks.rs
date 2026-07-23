//! Git hook integration — install/uninstall/status for atlas post-commit and
//! post-checkout hooks. Rust port of graphify's `hooks.py`.
//!
//! The absolute path of the atlas executable is embedded ("pinned") into the
//! generated hook at install time, so the hook fires even when atlas is not on
//! PATH at git-trigger time (GUI clients / CI often have a minimal PATH).
//!
//! Backlog #2126 / #2133: the interpreter allowlist must accept Windows
//! backslash paths (`C:\...\atlas.exe`). See [`sanitize_interpreter`] and its
//! regression test.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Result};

const HOOK_MARKER: &str = "# graphify-hook-start";
const HOOK_MARKER_END: &str = "# graphify-hook-end";
const CHECKOUT_MARKER: &str = "# graphify-checkout-hook-start";
const CHECKOUT_MARKER_END: &str = "# graphify-checkout-hook-end";

/// Return `exe` unchanged if every character is safe in a shell-embedded
/// filesystem path, else `None`.
///
/// The allowlist is `[A-Za-z0-9/_.@:-]` **plus backslash** — the `:` and `\`
/// are what make a Windows path like `C:\Program Files\...` pass rather than
/// being rejected as if it contained a shell metacharacter (#2126/#2133).
/// Anything outside the set (`;`, `$`, backtick, quote, space, ...) fails the
/// check, so the pinned path can never inject a command into the generated hook.
///
/// Note: a Windows path with spaces (e.g. `C:\Program Files\...`) still fails —
/// as in graphify, an unsafe pin degrades to the `atlas`-on-PATH fallback rather
/// than being quoted. #2126 is specifically about backslashes, not spaces.
pub fn sanitize_interpreter(exe: &str) -> Option<String> {
    let ok = exe.chars().all(|c| {
        c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '.' | '@' | ':' | '-' | '\\')
    });
    if exe.is_empty() || !ok {
        None
    } else {
        Some(exe.to_string())
    }
}

/// The pinned atlas executable path, or empty string if it fails the allowlist
/// (the hook then falls back to `atlas` on PATH).
fn pinned_exe() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(str::to_string))
        .and_then(|p| sanitize_interpreter(&p))
        .unwrap_or_default()
}

/// Build the post-commit hook body with the interpreter pinned in.
fn hook_script(pinned: &str) -> String {
    format!(
        "\
{HOOK_MARKER}
# Auto-rebuilds the knowledge graph after each commit. Installed by: atlas hook install
[ \"${{GRAPHIFY_SKIP_HOOK:-0}}\" = \"1\" ] && exit 0
GIT_DIR=${{GIT_DIR:-$(git rev-parse --git-dir 2>/dev/null)}}
[ -d \"$GIT_DIR/rebase-merge\" ] && exit 0
[ -d \"$GIT_DIR/rebase-apply\" ] && exit 0
[ -f \"$GIT_DIR/MERGE_HEAD\" ] && exit 0
CHANGED=$(git diff --name-only HEAD~1 HEAD 2>/dev/null || git diff --name-only HEAD 2>/dev/null)
[ -z \"$CHANGED\" ] && exit 0
# Pinned at install time so the hook works when atlas is not on PATH (uv/pipx/CI).
ATLAS='{pinned}'
[ -x \"$ATLAS\" ] || ATLAS=$(command -v atlas 2>/dev/null)
[ -n \"$ATLAS\" ] || {{ echo '[graphify hook] atlas not found on PATH' >&2; exit 0; }}
\"$ATLAS\" update . >/dev/null 2>&1 &
{HOOK_MARKER_END}
"
    )
}

/// Build the post-checkout hook body with the interpreter pinned in.
fn checkout_script(pinned: &str) -> String {
    format!(
        "\
{CHECKOUT_MARKER}
# Auto-rebuilds the knowledge graph on branch switch. Installed by: atlas hook install
[ \"${{GRAPHIFY_SKIP_HOOK:-0}}\" = \"1\" ] && exit 0
[ \"$3\" != \"1\" ] && exit 0
[ -d \"graphify-out\" ] || exit 0
ATLAS='{pinned}'
[ -x \"$ATLAS\" ] || ATLAS=$(command -v atlas 2>/dev/null)
[ -n \"$ATLAS\" ] || exit 0
\"$ATLAS\" update . >/dev/null 2>&1 &
{CHECKOUT_MARKER_END}
"
    )
}

/// Walk up from `path` to find the git repo root (a dir containing `.git`).
fn git_root(path: &Path) -> Option<PathBuf> {
    let current = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    std::iter::successors(Some(current.as_path()), |p| p.parent())
        .find(|p| p.join(".git").exists())
        .map(Path::to_path_buf)
}

fn hooks_dir(root: &Path) -> Result<PathBuf> {
    let d = root.join(".git").join("hooks");
    fs::create_dir_all(&d)?;
    Ok(d)
}

/// Install one hook, appending to any existing hook rather than clobbering it.
fn install_one(dir: &Path, name: &str, script: &str, marker: &str) -> Result<String> {
    let path = dir.join(name);
    if path.exists() {
        let content = fs::read_to_string(&path)?;
        if content.contains(marker) {
            return Ok(format!("already installed at {}", path.display()));
        }
        fs::write(&path, format!("{}\n\n{}", content.trim_end(), script))?;
        return Ok(format!("appended to existing {name} hook"));
    }
    fs::write(&path, format!("#!/bin/sh\n{script}"))?;
    set_executable(&path)?;
    Ok(format!("installed at {}", path.display()))
}

/// Strip the atlas section between markers; delete the hook if nothing else left.
fn uninstall_one(dir: &Path, name: &str, marker: &str, marker_end: &str) -> Result<String> {
    let path = dir.join(name);
    if !path.exists() {
        return Ok(format!("no {name} hook found"));
    }
    let content = fs::read_to_string(&path)?;
    if !content.contains(marker) {
        return Ok(format!("atlas hook not found in {name}"));
    }
    let re = regex::Regex::new(&format!(
        r"(?s){}.*?{}\n?",
        regex::escape(marker),
        regex::escape(marker_end)
    ))
    .unwrap();
    let stripped = re.replace_all(&content, "");
    let new = stripped.trim();
    if new.is_empty() || new == "#!/bin/sh" || new == "#!/bin/bash" {
        fs::remove_file(&path)?;
        return Ok(format!("removed {name} hook"));
    }
    fs::write(&path, format!("{new}\n"))?;
    Ok(format!(
        "atlas removed from {name} (other content preserved)"
    ))
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<()> {
    Ok(())
}

/// Install atlas post-commit and post-checkout hooks in the nearest git repo.
pub fn install(path: &Path) -> Result<String> {
    let root = match git_root(path) {
        Some(r) => r,
        None => bail!("No git repository found at or above {}", path.display()),
    };
    let dir = hooks_dir(&root)?;
    let pinned = pinned_exe();
    let commit = install_one(&dir, "post-commit", &hook_script(&pinned), HOOK_MARKER)?;
    let checkout = install_one(
        &dir,
        "post-checkout",
        &checkout_script(&pinned),
        CHECKOUT_MARKER,
    )?;
    Ok(format!("post-commit: {commit}\npost-checkout: {checkout}"))
}

/// Remove atlas post-commit and post-checkout hooks.
pub fn uninstall(path: &Path) -> Result<String> {
    let root = match git_root(path) {
        Some(r) => r,
        None => bail!("No git repository found at or above {}", path.display()),
    };
    let dir = hooks_dir(&root)?;
    let commit = uninstall_one(&dir, "post-commit", HOOK_MARKER, HOOK_MARKER_END)?;
    let checkout = uninstall_one(&dir, "post-checkout", CHECKOUT_MARKER, CHECKOUT_MARKER_END)?;
    Ok(format!("post-commit: {commit}\npost-checkout: {checkout}"))
}

/// Report whether atlas hooks are installed.
pub fn status(path: &Path) -> Result<String> {
    let root = match git_root(path) {
        Some(r) => r,
        None => return Ok("Not in a git repository.".to_string()),
    };
    let dir = hooks_dir(&root)?;
    let check = |name: &str, marker: &str| -> String {
        let p = dir.join(name);
        if !p.exists() {
            return "not installed".to_string();
        }
        match fs::read_to_string(&p) {
            Ok(c) if c.contains(marker) => "installed".to_string(),
            _ => "not installed (hook exists but atlas not found)".to_string(),
        }
    };
    Ok(format!(
        "post-commit: {}\npost-checkout: {}",
        check("post-commit", HOOK_MARKER),
        check("post-checkout", CHECKOUT_MARKER)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "atlas-hooks-{}-{}-{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// #2126/#2133 regression: Windows backslash paths must survive the allowlist.
    #[test]
    fn windows_backslash_allowlist() {
        assert_eq!(
            sanitize_interpreter(r"C:\Python311\python.exe").as_deref(),
            Some(r"C:\Python311\python.exe")
        );
        assert_eq!(
            sanitize_interpreter(r"D:\tools\atlas.exe").as_deref(),
            Some(r"D:\tools\atlas.exe")
        );
        // POSIX path still fine.
        assert_eq!(
            sanitize_interpreter("/usr/local/bin/atlas").as_deref(),
            Some("/usr/local/bin/atlas")
        );
        // Injection attempts rejected.
        assert!(sanitize_interpreter("atlas; rm -rf /").is_none());
        assert!(sanitize_interpreter("$(evil)").is_none());
        assert!(sanitize_interpreter("a`b`").is_none());
        assert!(sanitize_interpreter("").is_none());
    }

    #[test]
    fn install_status_uninstall_roundtrip() {
        let root = tmp("repo");
        fs::create_dir_all(root.join(".git")).unwrap();
        install(&root).unwrap();
        let pc = root.join(".git/hooks/post-commit");
        assert!(pc.exists());
        let body = fs::read_to_string(&pc).unwrap();
        assert!(body.contains(HOOK_MARKER));
        assert!(status(&root).unwrap().contains("post-commit: installed"));

        uninstall(&root).unwrap();
        assert!(!pc.exists());
        assert!(status(&root)
            .unwrap()
            .contains("post-commit: not installed"));
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn preserves_existing_hook_content() {
        let root = tmp("existing");
        let dir = root.join(".git/hooks");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("post-commit"), "#!/bin/sh\necho mine\n").unwrap();
        install(&root).unwrap();
        let body = fs::read_to_string(dir.join("post-commit")).unwrap();
        assert!(body.contains("echo mine"));
        assert!(body.contains(HOOK_MARKER));
        uninstall(&root).unwrap();
        let body = fs::read_to_string(dir.join("post-commit")).unwrap();
        assert!(body.contains("echo mine"));
        assert!(!body.contains(HOOK_MARKER));
        fs::remove_dir_all(&root).ok();
    }
}
