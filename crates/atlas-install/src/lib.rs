//! atlas assistant-skill installers + editor hooks.
//!
//! Rust port of graphify's `install.py`. Skill bodies are embedded in the binary
//! via `include_str!` (see `assets/`), so a single static binary carries every
//! payload with no runtime file dependency.
//!
//! Scope note (M9): the four core platforms — **claude**, **codex**, **cursor**,
//! **gemini** — are fully wired (skill file + platform-specific config: CLAUDE.md
//! registration, AGENTS.md + `.codex/hooks.json`, `.cursor/rules`, GEMINI.md).
//! Every other platform in [`PLATFORMS`] has a real target path and a generic
//! skill writer (skill file + version stamp only), but no bespoke config wiring.
//!
//! Path convention for hermetic testing: `install`/`uninstall` take a `base`
//! directory. When `project` is true it is the project root; when false it stands
//! in for the user's home directory. Tests always pass a temp dir, so the real
//! HOME is never touched.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

pub mod hooks;

// --- embedded skill bodies -------------------------------------------------
const SKILL_CLAUDE: &str = include_str!("../assets/skill-claude.md");
const SKILL_CODEX: &str = include_str!("../assets/skill-codex.md");
const AGENTS_MD: &str = include_str!("../assets/agents-md.md");
const GEMINI_MD: &str = include_str!("../assets/gemini-md.md");

const MARKER: &str = "## graphify";

/// `.cursor/rules/graphify.mdc` — wholly owned by atlas, overwritten on upgrade.
const CURSOR_RULE: &str = "\
---
description: graphify knowledge graph context
alwaysApply: true
---

This project has a graphify knowledge graph at graphify-out/.

**MANDATORY: Before using Read, Grep, Glob, or Bash to explore the codebase, you MUST run graphify first:**
- `graphify query \"<question>\"` — scoped subgraph for any codebase or architecture question
- `graphify path \"<A>\" \"<B>\"` — dependency path between two symbols
- `graphify explain \"<concept>\"` — all nodes related to a concept

This applies to YOU and to every subagent you spawn. Include this rule explicitly in every subagent prompt that involves code exploration.

Only use Read/Grep/Glob directly when graphify has already oriented you, or when `graphify-out/graph.json` does not exist yet.
";

// --- platform table --------------------------------------------------------

/// A destination for a platform's skill file, relative to a base directory.
pub struct PlatformCfg {
    pub name: &'static str,
    /// Embedded skill body written to the skill destination.
    pub skill_body: &'static str,
    /// Skill path relative to the project root (project scope).
    pub project_skill: &'static str,
    /// Skill path relative to the user's home dir (user scope).
    pub user_skill: &'static str,
}

/// Cursor has no SKILL.md — it is a rules file. Sentinel body so the table entry
/// still exposes a target path for the "every platform has a path" invariant.
const CURSOR_TARGET: &str = ".cursor/rules/graphify.mdc";

/// The platform table. Core-4 are fully wired in [`install`]; the rest fall
/// through to the generic skill writer (skill file + version stamp only).
pub static PLATFORMS: &[PlatformCfg] = &[
    PlatformCfg {
        name: "claude",
        skill_body: SKILL_CLAUDE,
        project_skill: ".claude/skills/graphify/SKILL.md",
        user_skill: ".claude/skills/graphify/SKILL.md",
    },
    PlatformCfg {
        name: "codex",
        skill_body: SKILL_CODEX,
        project_skill: ".codex/skills/graphify/SKILL.md",
        user_skill: ".codex/skills/graphify/SKILL.md",
    },
    PlatformCfg {
        name: "gemini",
        skill_body: SKILL_CLAUDE,
        project_skill: ".gemini/skills/graphify/SKILL.md",
        user_skill: ".gemini/skills/graphify/SKILL.md",
    },
    PlatformCfg {
        name: "cursor",
        skill_body: CURSOR_RULE,
        project_skill: CURSOR_TARGET,
        user_skill: CURSOR_TARGET,
    },
    // table-only: real target paths, generic writer, no bespoke config wiring yet.
    PlatformCfg {
        name: "opencode",
        skill_body: SKILL_CLAUDE,
        project_skill: ".opencode/skills/graphify/SKILL.md",
        user_skill: ".config/opencode/skills/graphify/SKILL.md",
    },
    PlatformCfg {
        name: "kilo",
        skill_body: SKILL_CLAUDE,
        project_skill: ".kilo/skills/graphify/SKILL.md",
        user_skill: ".config/kilo/skills/graphify/SKILL.md",
    },
    PlatformCfg {
        name: "aider",
        skill_body: SKILL_CLAUDE,
        project_skill: ".aider/graphify/SKILL.md",
        user_skill: ".aider/graphify/SKILL.md",
    },
    PlatformCfg {
        name: "copilot",
        skill_body: SKILL_CLAUDE,
        project_skill: ".copilot/skills/graphify/SKILL.md",
        user_skill: ".copilot/skills/graphify/SKILL.md",
    },
    PlatformCfg {
        name: "droid",
        skill_body: SKILL_CLAUDE,
        project_skill: ".factory/skills/graphify/SKILL.md",
        user_skill: ".factory/skills/graphify/SKILL.md",
    },
    PlatformCfg {
        name: "kiro",
        skill_body: SKILL_CLAUDE,
        project_skill: ".kiro/skills/graphify/SKILL.md",
        user_skill: ".kiro/skills/graphify/SKILL.md",
    },
    PlatformCfg {
        name: "amp",
        skill_body: SKILL_CLAUDE,
        project_skill: ".agents/skills/graphify/SKILL.md",
        user_skill: ".config/agents/skills/graphify/SKILL.md",
    },
    PlatformCfg {
        name: "agents",
        skill_body: SKILL_CLAUDE,
        project_skill: ".agents/skills/graphify/SKILL.md",
        user_skill: ".agents/skills/graphify/SKILL.md",
    },
    PlatformCfg {
        name: "devin",
        skill_body: SKILL_CLAUDE,
        project_skill: ".devin/skills/graphify/SKILL.md",
        user_skill: ".config/devin/skills/graphify/SKILL.md",
    },
];

/// CLI aliases resolved to a real table key.
fn canonical(name: &str) -> &str {
    match name {
        "skills" => "agents",
        other => other,
    }
}

/// Look up a platform config, error on unknown.
pub fn platform(name: &str) -> Result<&'static PlatformCfg> {
    let name = canonical(name);
    PLATFORMS.iter().find(|p| p.name == name).with_context(|| {
        let known: Vec<&str> = PLATFORMS.iter().map(|p| p.name).collect();
        format!(
            "unknown platform '{name}'. Choose from: {}",
            known.join(", ")
        )
    })
}

/// Resolve the skill destination for a platform under `base`.
///
/// `base` is the project root (project scope) or the user's home dir (user scope).
pub fn skill_destination(name: &str, base: &Path, project: bool) -> Result<PathBuf> {
    let cfg = platform(name)?;
    let rel = if project {
        cfg.project_skill
    } else {
        cfg.user_skill
    };
    Ok(base.join(rel))
}

// --- install ---------------------------------------------------------------

/// Install the assistant skill (and any platform-specific config) for `platform`.
pub fn install(platform_name: &str, base: &Path, project: bool) -> Result<()> {
    let cfg = platform(platform_name)?;

    if cfg.name == "cursor" {
        return write_cursor_rule(base);
    }

    let dst = base.join(if project {
        cfg.project_skill
    } else {
        cfg.user_skill
    });
    copy_skill(&dst, cfg.skill_body)?;

    match cfg.name {
        "claude" => register_claude_md(base, project)?,
        "codex" => {
            replace_or_append_section(&base.join("AGENTS.md"), MARKER, AGENTS_MD)?;
            install_codex_hook(base)?;
        }
        "gemini" => {
            replace_or_append_section(&base.join("GEMINI.md"), MARKER, GEMINI_MD)?;
        }
        _ => { /* generic writer: skill file only */ }
    }
    Ok(())
}

/// Uninstall whatever [`install`] wrote for `platform`.
pub fn uninstall(platform_name: &str, base: &Path, project: bool) -> Result<()> {
    let cfg = platform(platform_name)?;

    if cfg.name == "cursor" {
        let p = base.join(CURSOR_TARGET);
        if p.exists() {
            fs::remove_file(&p)?;
        }
        return Ok(());
    }

    let dst = base.join(if project {
        cfg.project_skill
    } else {
        cfg.user_skill
    });
    remove_skill(&dst)?;

    match cfg.name {
        "claude" => unregister_claude_md(base, project)?,
        "codex" => {
            remove_section(&base.join("AGENTS.md"), MARKER)?;
            uninstall_codex_hook(base)?;
        }
        "gemini" => {
            remove_section(&base.join("GEMINI.md"), MARKER)?;
        }
        _ => {}
    }
    Ok(())
}

// --- skill file I/O --------------------------------------------------------

fn copy_skill(dst: &Path, body: &str) -> Result<()> {
    let parent = dst.parent().context("skill destination has no parent")?;
    fs::create_dir_all(parent)?;
    fs::write(dst, body).with_context(|| format!("writing skill to {}", dst.display()))?;
    fs::write(parent.join(".graphify_version"), env!("CARGO_PKG_VERSION"))?;
    Ok(())
}

fn remove_skill(dst: &Path) -> Result<()> {
    if dst.exists() {
        fs::remove_file(dst)?;
    }
    if let Some(parent) = dst.parent() {
        let vf = parent.join(".graphify_version");
        if vf.exists() {
            fs::remove_file(vf)?;
        }
        // Walk up removing now-empty dirs (skills/graphify, skills, .claude, ...).
        let mut dir = Some(parent.to_path_buf());
        while let Some(d) = dir {
            if fs::remove_dir(&d).is_err() {
                break; // non-empty or gone
            }
            dir = d.parent().map(Path::to_path_buf);
        }
    }
    Ok(())
}

fn write_cursor_rule(base: &Path) -> Result<()> {
    let path = base.join(CURSOR_TARGET);
    fs::create_dir_all(path.parent().unwrap())?;
    fs::write(&path, CURSOR_RULE)?;
    Ok(())
}

// --- CLAUDE.md registration ------------------------------------------------

fn skill_registration(skill_path: &str) -> String {
    format!(
        "\n# graphify\n- **graphify** (`{skill_path}`) - any input to knowledge graph. Trigger: `/graphify`\nWhen the user types `/graphify`, use the installed graphify skill or instructions before doing anything else.\n"
    )
}

fn register_claude_md(base: &Path, project: bool) -> Result<()> {
    let claude_md = base.join(".claude").join("CLAUDE.md");
    let skill_path = if project {
        ".claude/skills/graphify/SKILL.md"
    } else {
        "~/.claude/skills/graphify/SKILL.md"
    };
    let reg = skill_registration(skill_path);
    if claude_md.exists() {
        let content = fs::read_to_string(&claude_md)?;
        if content.contains("graphify") {
            return Ok(()); // already registered
        }
        fs::write(&claude_md, format!("{}{}", content.trim_end(), reg))?;
    } else {
        fs::create_dir_all(claude_md.parent().unwrap())?;
        fs::write(&claude_md, reg.trim_start())?;
    }
    Ok(())
}

fn unregister_claude_md(base: &Path, _project: bool) -> Result<()> {
    let claude_md = base.join(".claude").join("CLAUDE.md");
    if !claude_md.exists() {
        return Ok(());
    }
    let content = fs::read_to_string(&claude_md)?;
    // The registration heading is an exact `# graphify` H1; strip to next H1/EOF.
    match remove_marker_section(&content, "# graphify", "# ") {
        None => Ok(()),
        Some(cleaned) if cleaned.is_empty() => {
            fs::remove_file(&claude_md)?;
            Ok(())
        }
        Some(cleaned) => {
            fs::write(&claude_md, format!("{cleaned}\n"))?;
            Ok(())
        }
    }
}

// --- codex .codex/hooks.json -----------------------------------------------

fn atlas_exe() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(str::to_string))
        .unwrap_or_else(|| "atlas".to_string())
}

fn install_codex_hook(base: &Path) -> Result<()> {
    let path = base.join(".codex").join("hooks.json");
    fs::create_dir_all(path.parent().unwrap())?;
    let mut root: serde_json::Value = if path.exists() {
        serde_json::from_str(&fs::read_to_string(&path)?).unwrap_or_else(|_| serde_json::json!({}))
    } else {
        serde_json::json!({})
    };
    let exe = atlas_exe();
    let entry = serde_json::json!({
        "matcher": "Bash",
        "hooks": [{"type": "command", "command": format!("{exe} hook-check")}]
    });
    let pre = pre_tool_use(&mut root);
    // Drop any prior atlas/graphify hook, then append the fresh one.
    pre.retain(|h| !h.to_string().contains("hook-check"));
    pre.push(entry);
    fs::write(&path, serde_json::to_string_pretty(&root)?)?;
    Ok(())
}

fn uninstall_codex_hook(base: &Path) -> Result<()> {
    let path = base.join(".codex").join("hooks.json");
    if !path.exists() {
        return Ok(());
    }
    let mut root: serde_json::Value = match serde_json::from_str(&fs::read_to_string(&path)?) {
        Ok(v) => v,
        Err(_) => return Ok(()),
    };
    let pre = pre_tool_use(&mut root);
    pre.retain(|h| !h.to_string().contains("hook-check"));
    fs::write(&path, serde_json::to_string_pretty(&root)?)?;
    Ok(())
}

/// `root["hooks"]["PreToolUse"]` as a mutable array, creating the path.
fn pre_tool_use(root: &mut serde_json::Value) -> &mut Vec<serde_json::Value> {
    let hooks = root
        .as_object_mut()
        .unwrap()
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}));
    let pre = hooks
        .as_object_mut()
        .unwrap()
        .entry("PreToolUse")
        .or_insert_with(|| serde_json::json!([]));
    if !pre.is_array() {
        *pre = serde_json::json!([]);
    }
    pre.as_array_mut().unwrap()
}

// --- shared markdown section helpers (ported from install.py) --------------

/// Idempotently replace or append an atlas-owned section in a shared markdown
/// file. The section is matched only when a line *is* exactly `marker` and runs
/// to the next `## ` heading or EOF. Creates the file if absent.
fn replace_or_append_section(path: &Path, marker: &str, new_section: &str) -> Result<()> {
    let content = if path.exists() {
        fs::read_to_string(path)?
    } else {
        if let Some(p) = path.parent() {
            fs::create_dir_all(p)?;
        }
        fs::write(path, new_section)?;
        return Ok(());
    };

    let lines: Vec<&str> = content.split('\n').collect();
    let starts: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.trim() == marker)
        .map(|(i, _)| i)
        .collect();

    let out = if starts.is_empty() {
        if content.trim().is_empty() {
            new_section.trim_start().to_string()
        } else {
            format!("{}\n\n{}", content.trim_end(), new_section.trim_start())
        }
    } else {
        let start = *starts.last().unwrap();
        let end = (start + 1..lines.len())
            .find(|&j| lines[j].starts_with("## "))
            .unwrap_or(lines.len());
        let head = lines[..start].join("\n").trim_end().to_string();
        let tail = lines[end..].join("\n").trim_start().to_string();
        let section = new_section.trim().to_string();
        let mut parts = Vec::new();
        if !head.is_empty() {
            parts.push(head);
        }
        parts.push(section);
        if !tail.is_empty() {
            parts.push(tail);
        }
        let mut o = parts.join("\n\n");
        if !o.ends_with('\n') {
            o.push('\n');
        }
        o
    };
    fs::write(path, out)?;
    Ok(())
}

/// Remove an atlas-owned section from a shared file (writes back, or deletes the
/// file if it becomes empty). No-op if the exact marker heading is absent.
fn remove_section(path: &Path, marker: &str) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let content = fs::read_to_string(path)?;
    match remove_marker_section(&content, marker, "## ") {
        None => Ok(()),
        Some(cleaned) if cleaned.is_empty() => {
            fs::remove_file(path)?;
            Ok(())
        }
        Some(cleaned) => {
            fs::write(path, format!("{cleaned}\n"))?;
            Ok(())
        }
    }
}

/// Remove every section whose heading line is exactly `marker`, each running to
/// the next `boundary_prefix` heading or EOF. Returns `None` when no exact
/// marker line exists (caller leaves the file untouched) — this guards against
/// stripping a substring mention or a deeper heading.
fn remove_marker_section(content: &str, marker: &str, boundary_prefix: &str) -> Option<String> {
    let mut lines: Vec<String> = content.split('\n').map(str::to_string).collect();
    let mut removed = false;
    loop {
        let start = match lines.iter().rposition(|l| l.trim() == marker) {
            Some(s) => s,
            None => break,
        };
        let end = (start + 1..lines.len())
            .find(|&j| lines[j].starts_with(boundary_prefix))
            .unwrap_or(lines.len());
        let head = lines[..start].join("\n").trim_end().to_string();
        let tail = lines[end..].join("\n").trim_start().to_string();
        let merged = if !head.is_empty() && !tail.is_empty() {
            format!("{head}\n\n{tail}")
        } else if !head.is_empty() {
            head
        } else {
            tail
        };
        lines = merged.split('\n').map(str::to_string).collect();
        removed = true;
    }
    if !removed {
        return None;
    }
    Some(lines.join("\n").trim_end().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "atlas-install-{}-{}-{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn claude_project_install_and_uninstall() {
        let root = tmp("claude");
        install("claude", &root, true).unwrap();
        let skill = root.join(".claude/skills/graphify/SKILL.md");
        assert!(skill.exists(), "SKILL.md written");
        let body = fs::read_to_string(&skill).unwrap();
        assert!(!body.trim().is_empty(), "embedded skill non-empty");
        assert!(
            body.contains("graphify"),
            "skill body looks like graphify content"
        );
        // CLAUDE.md registered.
        let cmd = root.join(".claude/CLAUDE.md");
        assert!(cmd.exists());
        assert!(fs::read_to_string(&cmd).unwrap().contains("# graphify"));

        uninstall("claude", &root, true).unwrap();
        assert!(!skill.exists(), "skill removed");
        // registration section stripped (file removed since it was only the reg).
        assert!(!cmd.exists() || !fs::read_to_string(&cmd).unwrap().contains("# graphify"));
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn cursor_install_writes_always_apply() {
        let root = tmp("cursor");
        install("cursor", &root, true).unwrap();
        let rule = root.join(".cursor/rules/graphify.mdc");
        let body = fs::read_to_string(&rule).unwrap();
        assert!(
            body.contains("alwaysApply: true"),
            "cursor rule has alwaysApply"
        );
        uninstall("cursor", &root, true).unwrap();
        assert!(!rule.exists());
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn codex_install_writes_agents_and_hooks() {
        let root = tmp("codex");
        install("codex", &root, true).unwrap();
        assert!(root.join(".codex/skills/graphify/SKILL.md").exists());
        assert!(fs::read_to_string(root.join("AGENTS.md"))
            .unwrap()
            .contains("## graphify"));
        let hooks: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(root.join(".codex/hooks.json")).unwrap())
                .unwrap();
        assert!(hooks["hooks"]["PreToolUse"].as_array().unwrap().len() >= 1);
        uninstall("codex", &root, true).unwrap();
        assert!(!root.join(".codex/skills/graphify/SKILL.md").exists());
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn gemini_install_writes_section() {
        let root = tmp("gemini");
        install("gemini", &root, true).unwrap();
        assert!(root.join(".gemini/skills/graphify/SKILL.md").exists());
        assert!(fs::read_to_string(root.join("GEMINI.md"))
            .unwrap()
            .contains("## graphify"));
        uninstall("gemini", &root, true).unwrap();
        assert!(
            !root.join("GEMINI.md").exists()
                || !fs::read_to_string(root.join("GEMINI.md"))
                    .unwrap()
                    .contains("## graphify")
        );
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn every_platform_has_a_target_path() {
        let root = std::env::temp_dir();
        for p in PLATFORMS {
            let dst = skill_destination(p.name, &root, true).unwrap();
            assert!(dst.starts_with(&root), "{} path under base", p.name);
            assert!(dst.to_str().unwrap().contains("graphify") || p.name == "cursor");
        }
    }

    #[test]
    fn unknown_platform_errors() {
        assert!(platform("nonesuch").is_err());
        assert!(install("nonesuch", &std::env::temp_dir(), true).is_err());
    }

    #[test]
    fn skills_alias_resolves_to_agents() {
        assert_eq!(platform("skills").unwrap().name, "agents");
    }

    #[test]
    fn replace_section_preserves_user_content() {
        let root = tmp("section");
        let f = root.join("AGENTS.md");
        fs::write(&f, "# My notes\n\nkeep me\n").unwrap();
        replace_or_append_section(&f, MARKER, "## graphify\n\ninjected\n").unwrap();
        let c = fs::read_to_string(&f).unwrap();
        assert!(c.contains("keep me"));
        assert!(c.contains("injected"));
        // idempotent: a re-run replaces in place, does not duplicate.
        replace_or_append_section(&f, MARKER, "## graphify\n\ninjected2\n").unwrap();
        let c = fs::read_to_string(&f).unwrap();
        assert_eq!(c.matches("## graphify").count(), 1);
        assert!(c.contains("injected2"));
        assert!(c.contains("keep me"));
        fs::remove_dir_all(&root).ok();
    }
}
