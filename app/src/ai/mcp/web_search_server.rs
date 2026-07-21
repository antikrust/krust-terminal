//! A built-in MCP server exposing one tool, `web_search`, so any harness
//! that lacks its own web search (chiefly tool-capable local Ollama models —
//! see `ai::agent_sdk::driver::harness::ollama_stream_exec`) can get one
//! without the user hand-configuring anything.
//!
//! Runs as a subprocess of this same binary (see [`run_stdio_server`] and its
//! caller in `main`), speaking MCP's stdio JSON-RPC framing directly rather
//! than depending on an external runtime. That keeps it a first-party app
//! feature — no Node, no separate install — matching how it's auto-installed
//! by [`super::templatable_manager`] rather than requiring the user to paste
//! JSON into the MCP Servers page.
//!
//! The backend is a keyless scrape of DuckDuckGo's HTML results page, the
//! same approach several open-source "search MCP servers" use. Fragile to
//! DDG markup changes and not meant for high request volume — a "rules of
//! thumb" integration in the same spirit as the Ollama catalog scraper.

use std::io::{BufRead, Write};

use regex::Regex;
use serde_json::{json, Value};

const WEB_SEARCH_TOOL_NAME: &str = "web_search";
const PROTOCOL_VERSION: &str = "2024-11-05";
const SERVER_NAME: &str = "warp-web-search";

/// Runs the server until stdin closes (the MCP client disconnects), then
/// returns. Called from the `web-search-mcp-server` worker subcommand (see
/// `warp_cli::WorkerCommand::WebSearchMcpServer`), which is this function's
/// entire reason to run — the process exits normally once it returns.
pub fn run_stdio_server() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to start a runtime for the web search MCP server");
    runtime.block_on(serve());
}

async fn serve() {
    let client = reqwest::Client::new();
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();

    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let Ok(request) = serde_json::from_str::<Value>(trimmed) else {
            eprintln!("warp-web-search: failed to parse line: {trimmed}");
            continue;
        };

        // Notifications (no `id`) need no response.
        let Some(id) = request.get("id").cloned() else {
            continue;
        };
        let method = request.get("method").and_then(Value::as_str).unwrap_or("");
        let params = request.get("params").cloned().unwrap_or(Value::Null);

        let response = handle_request(&client, method, &params, id).await;
        let _ = writeln!(stdout, "{response}");
        let _ = stdout.flush();
    }
}

async fn handle_request(
    client: &reqwest::Client,
    method: &str,
    params: &Value,
    id: Value,
) -> Value {
    match method {
        "initialize" => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION") },
            }
        }),
        "tools/list" => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": { "tools": [web_search_tool_definition()] },
        }),
        "tools/call" => call_tool(client, params, id).await,
        other => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32601, "message": format!("Method not found: {other}") },
        }),
    }
}

fn web_search_tool_definition() -> Value {
    json!({
        "name": WEB_SEARCH_TOOL_NAME,
        "description": "Search the public web and return the top results (title, URL, \
            snippet). Use for current events, prices, docs, or anything that may have \
            changed since the model was trained.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "The search query." },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum number of results to return (default 5, max 10).",
                },
            },
            "required": ["query"],
        },
    })
}

async fn call_tool(client: &reqwest::Client, params: &Value, id: Value) -> Value {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    if name != WEB_SEARCH_TOOL_NAME {
        return tool_result(id, true, format!("Unknown tool: {name}"));
    }

    let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);
    let query = arguments
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if query.is_empty() {
        return tool_result(id, true, "`query` must be a non-empty string.".to_string());
    }
    let max_results = arguments
        .get("max_results")
        .and_then(Value::as_u64)
        .unwrap_or(5)
        .clamp(1, 10) as usize;

    match search_duckduckgo(client, &query, max_results).await {
        Ok(results) => tool_result(id, false, format_results_as_text(&query, &results)),
        Err(error) => tool_result(id, true, format!("Search failed: {error}")),
    }
}

fn tool_result(id: Value, is_error: bool, text: String) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "isError": is_error,
            "content": [{ "type": "text", "text": text }],
        },
    })
}

pub(crate) struct SearchResult {
    title: String,
    url: String,
    snippet: String,
}

pub(crate) fn format_results_as_text(query: &str, results: &[SearchResult]) -> String {
    if results.is_empty() {
        return format!("No results found for \"{query}\".");
    }
    let lines: Vec<String> = results
        .iter()
        .enumerate()
        .map(|(index, result)| {
            format!(
                "{}. {}\n   {}\n   {}",
                index + 1,
                result.title,
                result.url,
                result.snippet
            )
        })
        .collect();
    format!("Search results for \"{query}\":\n\n{}", lines.join("\n\n"))
}

async fn search_duckduckgo(
    client: &reqwest::Client,
    query: &str,
    max_results: usize,
) -> anyhow::Result<Vec<SearchResult>> {
    let response = client
        .post("https://html.duckduckgo.com/html/")
        .header(
            "User-Agent",
            "Mozilla/5.0 (compatible; WarpWebSearchMcp/1.0)",
        )
        .form(&[("q", query)])
        .send()
        .await?
        .error_for_status()?;
    let html = response.text().await?;
    Ok(parse_duckduckgo_html(&html, max_results))
}

/// Extracts result links/titles and snippets from DuckDuckGo's HTML results
/// page. Loose by design (regex over the raw markup rather than a full HTML
/// parser dependency): keys off the two stable `result__a` / `result__snippet`
/// classes DDG's lite/html endpoint has used for years, not on surrounding
/// structure, which is the part likeliest to change.
pub(crate) fn parse_duckduckgo_html(html: &str, max_results: usize) -> Vec<SearchResult> {
    let link_pattern =
        Regex::new(r#"(?s)<a[^>]*class="result__a"[^>]*href="([^"]+)"[^>]*>(.*?)</a>"#)
            .expect("valid regex");
    let snippet_pattern =
        Regex::new(r#"(?s)<a[^>]*class="result__snippet"[^>]*>(.*?)</a>"#).expect("valid regex");

    let titles: Vec<(String, String)> = link_pattern
        .captures_iter(html)
        .map(|caps| {
            (
                resolve_result_url(&caps[1]),
                strip_tags(decode_html_entities(&caps[2])),
            )
        })
        .collect();
    let snippets: Vec<String> = snippet_pattern
        .captures_iter(html)
        .map(|caps| strip_tags(decode_html_entities(&caps[1])))
        .collect();

    titles
        .into_iter()
        .zip(snippets.into_iter().chain(std::iter::repeat(String::new())))
        .take(max_results)
        .map(|((url, title), snippet)| SearchResult {
            title,
            url,
            snippet,
        })
        .collect()
}

/// Unwraps DuckDuckGo's `//duckduckgo.com/l/?uddg=<encoded-url>&rut=...`
/// tracking redirect so the model gets a usable URL.
fn resolve_result_url(href: &str) -> String {
    let absolute = if href.starts_with("//") {
        format!("https:{href}")
    } else {
        href.to_string()
    };
    url::Url::parse(&absolute)
        .ok()
        .and_then(|parsed| {
            parsed
                .query_pairs()
                .find(|(key, _)| key == "uddg")
                .map(|(_, value)| value.into_owned())
        })
        .unwrap_or(absolute)
}

fn decode_html_entities(text: &str) -> String {
    text.replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}

fn strip_tags(html: String) -> String {
    static TAG_PATTERN: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let pattern = TAG_PATTERN.get_or_init(|| Regex::new(r"<[^>]+>").expect("valid regex"));
    pattern.replace_all(&html, "").trim().to_string()
}

#[cfg(test)]
#[path = "web_search_server_tests.rs"]
mod tests;
