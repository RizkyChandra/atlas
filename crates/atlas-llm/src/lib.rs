//! atlas-llm — direct-API LLM backends, a Rust port of graphify's `llm.py`.
//!
//! Ports the `BACKENDS` table, env-var auto-detection priority, per-family
//! request/response shaping, and 429 retry/backoff. Covers the 8 HTTP backends
//! (claude, openai, gemini, kimi, deepseek, ollama, azure, bedrock) plus the
//! `claude-cli` subprocess path. The Python pipeline normally routes through
//! Claude Code subagents; this is the direct path for non-agent environments.

use std::env;
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};

const DEFAULT_MAX_TOKENS: u32 = 16384;
const DEFAULT_MAX_RETRIES: u32 = 6;
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Backend call-path family. Determines URL, auth header, and request/response
/// JSON shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// OpenAI-compatible `/chat/completions` (openai, gemini, kimi, deepseek, ollama).
    OpenAiCompat,
    /// Azure OpenAI — same body as OpenAiCompat but deployment-in-URL + `api-key` header.
    Azure,
    /// Anthropic Messages API (`/v1/messages`, `x-api-key`).
    Anthropic,
    /// AWS Bedrock Converse. Body is built for testing; sending needs SigV4 (gap).
    Bedrock,
    /// Local `claude` CLI subprocess (`claude -p --output-format json`).
    ClaudeCli,
}

/// A resolved backend: everything needed to build and send one completion call.
#[derive(Debug, Clone)]
pub struct Backend {
    pub name: String,
    pub kind: Kind,
    /// Base URL (OpenAI/Anthropic) or endpoint (Azure). Empty for bedrock/claude-cli.
    pub base_url: String,
    pub model: String,
    pub api_key: String,
    pub max_tokens: u32,
    /// `None` means "omit temperature" (kimi enforces its own; reasoning models 400 on any value).
    pub temperature: Option<f64>,
    /// Azure API version (`api-version` query param). Empty for other kinds.
    pub api_version: String,
}

// ── env helpers ───────────────────────────────────────────────────────────────

fn env_opt(key: &str) -> Option<String> {
    env::var(key)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn env_or(key: &str, default: &str) -> String {
    env_opt(key).unwrap_or_else(|| default.to_string())
}

fn first_env(keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|k| env_opt(k))
}

/// Honour `GRAPHIFY_MAX_OUTPUT_TOKENS` override, else backend default.
fn resolve_max_tokens() -> u32 {
    env_opt("GRAPHIFY_MAX_OUTPUT_TOKENS")
        .and_then(|s| s.parse::<u32>().ok())
        .filter(|&v| v > 0)
        .unwrap_or(DEFAULT_MAX_TOKENS)
}

/// Retry budget for transient 429s. Honours `GRAPHIFY_MAX_RETRIES` (0 disables).
pub fn resolve_max_retries() -> u32 {
    env_opt("GRAPHIFY_MAX_RETRIES")
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(DEFAULT_MAX_RETRIES)
}

/// Resolve the Ollama base URL from `OLLAMA_BASE_URL` (verbatim) or `OLLAMA_HOST`
/// (normalized). `None` when neither is set — so ollama stays opt-in and never
/// shadows a paid key during detection.
// ponytail: skips OLLAMA_HOST port-default (11434) injection; add if a bare-host
// OLLAMA_HOST that resolves to :80 becomes a real complaint.
fn resolve_ollama_base_url() -> Option<String> {
    if let Some(u) = env_opt("OLLAMA_BASE_URL") {
        return Some(u);
    }
    let mut h = env_opt("OLLAMA_HOST")?;
    if h.chars().all(|c| c.is_ascii_digit()) {
        h = format!("localhost:{h}");
    } else if h.starts_with(':') && h[1..].chars().all(|c| c.is_ascii_digit()) {
        h = format!("localhost{h}");
    }
    if !(h.starts_with("http://") || h.starts_with("https://")) {
        h = format!("http://{h}");
    }
    let h = h.trim_end_matches('/').to_string();
    Some(if h.ends_with("/v1") {
        h
    } else {
        format!("{h}/v1")
    })
}

// ── backend table (ports graphify BACKENDS) ────────────────────────────────────

/// Build a backend by name, resolving base_url/model/key from the env exactly as
/// graphify's `BACKENDS` table does. Errors on an unknown name.
pub fn build(name: &str) -> Result<Backend> {
    let max_tokens = resolve_max_tokens();
    let mk = |kind, base_url: String, model: String, api_key: String, temperature| Backend {
        name: name.to_string(),
        kind,
        base_url,
        model,
        api_key,
        max_tokens,
        temperature,
        api_version: String::new(),
    };
    Ok(match name {
        "gemini" => mk(
            Kind::OpenAiCompat,
            env_or(
                "GEMINI_BASE_URL",
                "https://generativelanguage.googleapis.com/v1beta/openai/",
            ),
            env_opt("GRAPHIFY_GEMINI_MODEL").unwrap_or_else(|| "gemini-3-flash-preview".into()),
            first_env(&["GEMINI_API_KEY", "GOOGLE_API_KEY"]).unwrap_or_default(),
            Some(0.0),
        ),
        "kimi" => mk(
            Kind::OpenAiCompat,
            env_or("KIMI_BASE_URL", "https://api.moonshot.ai/v1"),
            "kimi-k2.6".into(),
            env_or("MOONSHOT_API_KEY", ""),
            None, // kimi-k2.6 enforces a fixed temperature; sending any value 400s.
        ),
        "claude" => mk(
            Kind::Anthropic,
            env_or("ANTHROPIC_BASE_URL", "https://api.anthropic.com"),
            env_or("ANTHROPIC_MODEL", "claude-sonnet-4-6"),
            env_or("ANTHROPIC_API_KEY", ""),
            Some(0.0),
        ),
        "openai" => mk(
            Kind::OpenAiCompat,
            env_or("OPENAI_BASE_URL", "https://api.openai.com/v1"),
            first_env(&["GRAPHIFY_OPENAI_MODEL", "OPENAI_MODEL"])
                .unwrap_or_else(|| "gpt-4.1-mini".into()),
            env_or("OPENAI_API_KEY", ""),
            Some(0.0),
        ),
        "deepseek" => mk(
            Kind::OpenAiCompat,
            env_or("DEEPSEEK_BASE_URL", "https://api.deepseek.com"),
            env_opt("GRAPHIFY_DEEPSEEK_MODEL").unwrap_or_else(|| "deepseek-v4-flash".into()),
            env_or("DEEPSEEK_API_KEY", ""),
            Some(0.0),
        ),
        "ollama" => mk(
            Kind::OpenAiCompat,
            resolve_ollama_base_url().unwrap_or_else(|| "http://localhost:11434/v1".into()),
            env_or("OLLAMA_MODEL", "qwen2.5-coder:7b"),
            // Ollama ignores auth but the OpenAI client requires a non-empty key.
            env_or("OLLAMA_API_KEY", "ollama"),
            Some(0.0),
        ),
        "azure" => {
            let mut b = mk(
                Kind::Azure,
                env_or("AZURE_OPENAI_ENDPOINT", ""),
                first_env(&["GRAPHIFY_AZURE_MODEL", "AZURE_OPENAI_DEPLOYMENT"])
                    .unwrap_or_else(|| "gpt-4o".into()),
                env_or("AZURE_OPENAI_API_KEY", ""),
                Some(0.0),
            );
            b.api_version = env_or("AZURE_OPENAI_API_VERSION", "2024-12-01-preview");
            b
        }
        "bedrock" => mk(
            Kind::Bedrock,
            String::new(),
            env_opt("GRAPHIFY_BEDROCK_MODEL")
                .unwrap_or_else(|| "anthropic.claude-3-5-sonnet-20241022-v2:0".into()),
            String::new(),
            Some(0.0),
        ),
        "claude-cli" => mk(
            Kind::ClaudeCli,
            String::new(),
            "claude-code-plan".into(),
            String::new(),
            Some(0.0),
        ),
        other => bail!("unknown backend {other:?}"),
    })
}

/// Auto-detect the backend from the environment in graphify's priority order:
/// gemini → kimi → claude → openai → deepseek → azure → bedrock → ollama.
///
/// Ollama is checked LAST so an incidental `OLLAMA_BASE_URL` never shadows a paid
/// key (graphify F-002/F-029). Errors listing what's missing when none is set.
pub fn detect_backend() -> Result<Backend> {
    if first_env(&["GEMINI_API_KEY", "GOOGLE_API_KEY"]).is_some() {
        return build("gemini");
    }
    if env_opt("MOONSHOT_API_KEY").is_some() {
        return build("kimi");
    }
    if env_opt("ANTHROPIC_API_KEY").is_some() {
        return build("claude");
    }
    if env_opt("OPENAI_API_KEY").is_some() {
        return build("openai");
    }
    if env_opt("DEEPSEEK_API_KEY").is_some() {
        return build("deepseek");
    }
    if env_opt("AZURE_OPENAI_API_KEY").is_some() && env_opt("AZURE_OPENAI_ENDPOINT").is_some() {
        return build("azure");
    }
    if first_env(&["AWS_PROFILE", "AWS_REGION", "AWS_DEFAULT_REGION"]).is_some() {
        return build("bedrock");
    }
    if resolve_ollama_base_url().is_some() {
        return build("ollama");
    }
    bail!(
        "No LLM backend configured. Set one of: GEMINI_API_KEY, ANTHROPIC_API_KEY, \
         OPENAI_API_KEY, DEEPSEEK_API_KEY, MOONSHOT_API_KEY, \
         AZURE_OPENAI_API_KEY+AZURE_OPENAI_ENDPOINT, OLLAMA_BASE_URL, \
         or AWS credentials (AWS_PROFILE/AWS_REGION)."
    )
}

// ── request/response shaping ───────────────────────────────────────────────────

impl Backend {
    /// The full request URL for this backend's family.
    pub fn url(&self) -> String {
        let base = self.base_url.trim_end_matches('/');
        match self.kind {
            Kind::OpenAiCompat => format!("{base}/chat/completions"),
            Kind::Azure => format!(
                "{base}/openai/deployments/{}/chat/completions?api-version={}",
                self.model, self.api_version
            ),
            Kind::Anthropic => format!("{base}/v1/messages"),
            Kind::Bedrock | Kind::ClaudeCli => String::new(),
        }
    }

    /// Build the serialized request body for `(system, user)`, matching the
    /// per-family shape graphify sends. `Null` for the claude-cli subprocess path.
    pub fn build_body(&self, system: &str, user: &str) -> Value {
        match self.kind {
            Kind::OpenAiCompat | Kind::Azure => {
                let mut b = json!({
                    "model": self.model,
                    "messages": [
                        {"role": "system", "content": system},
                        {"role": "user", "content": user},
                    ],
                    "max_completion_tokens": self.max_tokens,
                    "stream": false,
                });
                if let Some(t) = self.temperature {
                    b["temperature"] = json!(t);
                }
                b
            }
            Kind::Anthropic => json!({
                "model": self.model,
                "max_tokens": self.max_tokens,
                "system": system,
                "messages": [{"role": "user", "content": user}],
            }),
            Kind::Bedrock => {
                let mut infer = json!({ "maxTokens": self.max_tokens });
                if let Some(t) = self.temperature {
                    infer["temperature"] = json!(t);
                }
                json!({
                    "system": [{"text": system}],
                    "messages": [{"role": "user", "content": [{"text": user}]}],
                    "inferenceConfig": infer,
                })
            }
            Kind::ClaudeCli => Value::Null,
        }
    }

    /// Run one completion, honouring `GRAPHIFY_MAX_RETRIES` for 429 backoff.
    pub fn complete(&self, system: &str, user: &str) -> Result<String> {
        self.complete_with(system, user, resolve_max_retries())
    }

    /// Run one completion with an explicit retry budget (test seam — avoids the
    /// process-global env var).
    pub fn complete_with(&self, system: &str, user: &str, max_retries: u32) -> Result<String> {
        match self.kind {
            Kind::ClaudeCli => return complete_claude_cli(system, user),
            // ponytail: Bedrock Converse needs AWS SigV4 request signing; the body
            // shape is ported and tested, sending is deferred. Add reqwest+aws-sigv4
            // (or the aws-sdk-bedrockruntime crate) when a live Bedrock path is needed.
            Kind::Bedrock => bail!(
                "bedrock backend requires AWS SigV4 signing (not implemented; \
                 build_body() produces the Converse shape for testing)"
            ),
            _ => {}
        }
        let body = self.build_body(system, user);
        let url = self.url();
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(600))
            .build()?;
        let mut attempt = 0u32;
        loop {
            let req = client.post(&url).json(&body);
            let req = match self.kind {
                Kind::Anthropic => req
                    .header("x-api-key", &self.api_key)
                    .header("anthropic-version", ANTHROPIC_VERSION),
                Kind::Azure => req.header("api-key", &self.api_key),
                _ => req.header("Authorization", format!("Bearer {}", self.api_key)),
            };
            let resp = req.send()?;
            if resp.status().as_u16() == 429 {
                if attempt >= max_retries {
                    bail!("rate limited (429) after {max_retries} retries");
                }
                let wait = retry_after(&resp).unwrap_or_else(|| backoff(attempt));
                attempt += 1;
                std::thread::sleep(wait);
                continue;
            }
            let resp = resp.error_for_status()?;
            let v: Value = resp.json()?;
            return extract_content(self.kind, &v);
        }
    }
}

/// Parse `Retry-After` (delta-seconds only) into a delay.
fn retry_after(resp: &reqwest::blocking::Response) -> Option<Duration> {
    let raw = resp
        .headers()
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?;
    let secs: f64 = raw.trim().parse().ok()?;
    Some(Duration::from_secs_f64(secs.max(0.0)))
}

/// Exponential backoff when the server gives no Retry-After. Capped at ~32s.
fn backoff(attempt: u32) -> Duration {
    Duration::from_millis(500u64 * 2u64.pow(attempt.min(6)))
}

/// Pull the assistant text out of a parsed response per family.
fn extract_content(kind: Kind, v: &Value) -> Result<String> {
    let text = match kind {
        Kind::Anthropic => v["content"][0]["text"].as_str(),
        _ => v["choices"][0]["message"]["content"].as_str(),
    };
    text.map(|s| s.to_string())
        .ok_or_else(|| anyhow!("response missing content: {v}"))
}

// ── claude CLI subprocess backend ──────────────────────────────────────────────

/// Call the locally-installed `claude` CLI (`claude -p --output-format json`),
/// authenticating via the user's Claude Code subscription instead of an API key.
fn complete_claude_cli(system: &str, user: &str) -> Result<String> {
    let combined = format!("{system}\n\n---\n{user}");
    let mut child = Command::new("claude")
        .args(["-p", "--output-format", "json", "--no-session-persistence"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            anyhow!("claude CLI not found on $PATH ({e}); install from https://claude.ai/code")
        })?;
    child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("failed to open claude stdin"))?
        .write_all(combined.as_bytes())?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!(
            "claude -p exited {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    claude_cli_result(&String::from_utf8_lossy(&out.stdout))
}

/// Extract the `result` string from a `claude -p --output-format json` envelope.
/// Newer CLIs emit a JSON array of stream events with a final `{"type":"result"}`;
/// older ones a single object. Normalise both.
fn claude_cli_result(stdout: &str) -> Result<String> {
    let env: Value = serde_json::from_str(stdout.trim())
        .map_err(|e| anyhow!("claude -p produced unparseable JSON envelope: {e}"))?;
    let obj = match &env {
        Value::Array(events) => events
            .iter()
            .rev()
            .find(|e| e.get("type").and_then(|t| t.as_str()) == Some("result"))
            .or_else(|| events.last())
            .ok_or_else(|| anyhow!("claude -p returned an empty JSON array"))?,
        other => other,
    };
    obj.get("result")
        .and_then(|r| r.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow!("claude -p envelope missing `result`"))
}

#[cfg(test)]
mod tests;
