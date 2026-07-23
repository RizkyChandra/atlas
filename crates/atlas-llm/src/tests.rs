use super::*;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

// ── detection / priority ───────────────────────────────────────────────────────

const ALL_ENV: &[&str] = &[
    "GEMINI_API_KEY", "GOOGLE_API_KEY", "MOONSHOT_API_KEY", "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY", "DEEPSEEK_API_KEY", "AZURE_OPENAI_API_KEY", "AZURE_OPENAI_ENDPOINT",
    "AWS_PROFILE", "AWS_REGION", "AWS_DEFAULT_REGION", "OLLAMA_BASE_URL", "OLLAMA_HOST",
];

fn clear_all() {
    for k in ALL_ENV {
        env::remove_var(k);
    }
}

// One test drives all detection so the process-global env is never mutated by
// two tests at once.
#[test]
fn detect_backend_priority_and_error() {
    clear_all();
    // None set -> documented error listing the missing vars.
    let err = detect_backend().unwrap_err().to_string();
    assert!(err.contains("No LLM backend configured"), "{err}");
    assert!(err.contains("GEMINI_API_KEY") && err.contains("AWS credentials"), "{err}");

    // Ollama last: an incidental OLLAMA_BASE_URL alone selects ollama...
    env::set_var("OLLAMA_BASE_URL", "http://localhost:11434/v1");
    assert_eq!(detect_backend().unwrap().name, "ollama");
    // ...but a paid key wins over it (priority above ollama).
    env::set_var("AWS_REGION", "us-east-1");
    assert_eq!(detect_backend().unwrap().name, "bedrock");
    env::set_var("AZURE_OPENAI_API_KEY", "az");
    env::set_var("AZURE_OPENAI_ENDPOINT", "https://x.openai.azure.com");
    assert_eq!(detect_backend().unwrap().name, "azure");
    env::set_var("DEEPSEEK_API_KEY", "ds");
    assert_eq!(detect_backend().unwrap().name, "deepseek");
    env::set_var("OPENAI_API_KEY", "oa");
    assert_eq!(detect_backend().unwrap().name, "openai");
    env::set_var("ANTHROPIC_API_KEY", "an");
    assert_eq!(detect_backend().unwrap().name, "claude");
    env::set_var("MOONSHOT_API_KEY", "mo");
    assert_eq!(detect_backend().unwrap().name, "kimi");
    env::set_var("GOOGLE_API_KEY", "go"); // gemini accepts GOOGLE_API_KEY too
    assert_eq!(detect_backend().unwrap().name, "gemini");

    // Azure needs BOTH key and endpoint — key alone does not select it.
    clear_all();
    env::set_var("AZURE_OPENAI_API_KEY", "az");
    let err = detect_backend().unwrap_err().to_string();
    assert!(err.contains("No LLM backend configured"), "{err}");

    // OLLAMA_HOST (bare host) is honoured and normalized to an OpenAI-compat /v1 URL.
    clear_all();
    env::set_var("OLLAMA_HOST", "10.0.0.9:11434");
    let b = detect_backend().unwrap();
    assert_eq!(b.name, "ollama");
    assert_eq!(b.base_url, "http://10.0.0.9:11434/v1");
    clear_all();
}

// ── request-body construction per family ───────────────────────────────────────

fn mk(name: &str, kind: Kind, base_url: &str, model: &str) -> Backend {
    Backend {
        name: name.into(),
        kind,
        base_url: base_url.into(),
        model: model.into(),
        api_key: "k".into(),
        max_tokens: 16384,
        temperature: Some(0.0),
        api_version: String::new(),
    }
}

#[test]
fn openai_family_body_shape() {
    let b = mk("openai", Kind::OpenAiCompat, "https://api.openai.com/v1", "gpt-4.1-mini");
    let body = b.build_body("SYS", "USR");
    assert_eq!(
        body,
        json!({
            "model": "gpt-4.1-mini",
            "messages": [
                {"role": "system", "content": "SYS"},
                {"role": "user", "content": "USR"},
            ],
            "max_completion_tokens": 16384,
            "stream": false,
            "temperature": 0.0,
        })
    );
    assert_eq!(b.url(), "https://api.openai.com/v1/chat/completions");
}

#[test]
fn kimi_omits_temperature() {
    let mut b = mk("kimi", Kind::OpenAiCompat, "https://api.moonshot.ai/v1", "kimi-k2.6");
    b.temperature = None;
    let body = b.build_body("SYS", "USR");
    assert!(body.get("temperature").is_none(), "kimi must omit temperature: {body}");
}

#[test]
fn anthropic_body_shape() {
    let b = mk("claude", Kind::Anthropic, "https://api.anthropic.com", "claude-sonnet-4-6");
    let body = b.build_body("SYS", "USR");
    assert_eq!(
        body,
        json!({
            "model": "claude-sonnet-4-6",
            "max_tokens": 16384,
            "system": "SYS",
            "messages": [{"role": "user", "content": "USR"}],
        })
    );
    assert_eq!(b.url(), "https://api.anthropic.com/v1/messages");
}

#[test]
fn azure_url_and_bedrock_body() {
    let mut b = mk("azure", Kind::Azure, "https://x.openai.azure.com", "gpt-4o");
    b.api_version = "2024-12-01-preview".into();
    assert_eq!(
        b.url(),
        "https://x.openai.azure.com/openai/deployments/gpt-4o/chat/completions?api-version=2024-12-01-preview"
    );

    let bd = mk("bedrock", Kind::Bedrock, "", "anthropic.claude-3-5-sonnet-20241022-v2:0");
    let body = bd.build_body("SYS", "USR");
    assert_eq!(
        body,
        json!({
            "system": [{"text": "SYS"}],
            "messages": [{"role": "user", "content": [{"text": "USR"}]}],
            "inferenceConfig": {"maxTokens": 16384, "temperature": 0.0},
        })
    );
}

// ── retry / backoff (local TcpListener stub, no real network) ───────────────────

/// A one-off HTTP stub: reply `first` to the first `repeat_first` requests, then
/// `then` to the rest. Returns (base_url, request-counter). Threads self-clean.
fn stub_server(first: &'static str, repeat_first: usize, then: &'static str) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let hits = Arc::new(AtomicUsize::new(0));
    let hits2 = hits.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = match stream {
                Ok(s) => s,
                Err(_) => break,
            };
            // Drain the request (headers + Content-Length body) so the client's
            // write completes before we respond.
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let n = hits2.fetch_add(1, Ordering::SeqCst);
            let payload = if n < repeat_first { first } else { then };
            let _ = stream.write_all(payload.as_bytes());
            let _ = stream.flush();
        }
    });
    (format!("http://127.0.0.1:{port}"), hits)
}

const RESP_429: &str = "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 0\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

fn resp_200() -> &'static str {
    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 42\r\nConnection: close\r\n\r\n\
     {\"choices\":[{\"message\":{\"content\":\"OK\"}}]}"
}

#[test]
fn retry_429_then_200_succeeds() {
    let (base, hits) = stub_server(RESP_429, 1, resp_200());
    let b = mk("openai", Kind::OpenAiCompat, &base, "m");
    let out = b.complete_with("s", "u", 6).unwrap();
    assert_eq!(out, "OK");
    assert_eq!(hits.load(Ordering::SeqCst), 2, "one 429 then one 200");
}

#[test]
fn retry_honors_max_cap() {
    // Always 429; with max_retries=1 we expect exactly 2 attempts then failure.
    let (base, hits) = stub_server(RESP_429, usize::MAX, RESP_429);
    let b = mk("openai", Kind::OpenAiCompat, &base, "m");
    let err = b.complete_with("s", "u", 1).unwrap_err().to_string();
    assert!(err.contains("rate limited"), "{err}");
    assert_eq!(hits.load(Ordering::SeqCst), 2, "initial + 1 retry = 2 attempts");
}

#[test]
fn claude_cli_result_parses_array_and_object() {
    // Newer CLI: array of events with a final result object.
    let arr = r#"[{"type":"system"},{"type":"result","result":"HELLO"}]"#;
    assert_eq!(claude_cli_result(arr).unwrap(), "HELLO");
    // Older CLI: single object.
    let obj = r#"{"result":"HI","type":"result"}"#;
    assert_eq!(claude_cli_result(obj).unwrap(), "HI");
}
