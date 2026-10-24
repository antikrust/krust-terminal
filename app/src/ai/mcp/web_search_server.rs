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
//! same approach several open-source "search MCP servers" use, with a Bing
//! HTML fallback when DuckDuckGo returns nothing or rate-limits us. Fragile to
//! either engine's markup changes and not meant for high request volume — a
//! "rules of thumb" integration in the same spirit as the Ollama catalog
//! scraper.

use std::io::{BufRead, Write};

use base64::Engine;
use regex::Regex;
use serde_json::{json, Value};

const WEB_SEARCH_TOOL_NAME: &str = "web_search";
const PROTOCOL_VERSION: &str = "2024-11-05";
const SERVER_NAME: &str = "krust-web-search";

/// A real browser User-Agent. Both engines serve a bot-challenge (an empty
/// `anomaly.js` page from DuckDuckGo, a 403 from others) much more readily to
/// an obviously-automated agent string like the old `KrustWebSearchMcp/1.0`,
/// which is why searches were coming back empty. A mainstream desktop Chrome
/// string gets ordinary results.
const BROWSER_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
    AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36";

/// Runs the server until stdin closes (the MCP client disconnects), then
/// returns. Called from the `web-search-mcp-server` worker subcommand (see
/// `krust_cli::WorkerCommand::WebSearchMcpServer`), which is this function's
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
            eprintln!("krust-web-search: failed to parse line: {trimmed}");
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

    let (text, is_error) = run_search(client, &query, max_results).await;
    tool_result(id, is_error, text)
}

/// The three ways a single engine query can turn out. Kept distinct so the
/// model is told the truth: a blocked search ("retry shortly") must not read
/// as a genuinely empty one ("there is nothing"), or a weak model fires the
/// same query again and again (eight times, in one observed session) treating
/// a rate-limit as a dead end.
enum SearchOutcome {
    /// The engine answered and we parsed results.
    Results(Vec<SearchResult>),
    /// The engine answered normally but with no matches.
    Empty,
    /// The engine refused, rate-limited, or served a bot challenge.
    Blocked,
}

/// Searches DuckDuckGo, falling back to Bing when DuckDuckGo returns nothing
/// or blocks us. Returns the text to hand the model and whether to flag it as
/// an error (true only when both engines were blocked/unreachable, never for a
/// genuine no-results).
async fn run_search(
    client: &reqwest::Client,
    query: &str,
    max_results: usize,
) -> (String, bool) {
    let primary = fetch_engine(client, Engine_::DuckDuckGo, query, max_results).await;
    if let SearchOutcome::Results(results) = &primary {
        return (format_results_as_text(query, results), false);
    }

    let fallback = fetch_engine(client, Engine_::Bing, query, max_results).await;
    if let SearchOutcome::Results(results) = &fallback {
        return (format_results_as_text(query, results), false);
    }

    // Neither engine returned results. If either was merely empty (a real
    // search that matched nothing), say so plainly; only if both were blocked
    // do we report a transient failure the model should retry rather than
    // treat as fact.
    let any_empty = matches!(primary, SearchOutcome::Empty)
        || matches!(fallback, SearchOutcome::Empty);
    if any_empty {
        (format!("No results found for \"{query}\"."), false)
    } else {
        (
            format!(
                "Web search is temporarily unavailable (the search engines rate-limited this \
                 request). This is not a result about \"{query}\" — wait a few seconds and try the \
                 same search again, or answer from what you already know."
            ),
            true,
        )
    }
}

/// Which search engine a fetch targets.
#[derive(Clone, Copy)]
enum Engine_ {
    DuckDuckGo,
    Bing,
}

/// Runs one engine query and classifies the outcome. A network error or a
/// non-success HTTP status is [`SearchOutcome::Blocked`] (the caller treats it
/// the same as a bot challenge); a 2xx page that parses to no results is
/// [`SearchOutcome::Empty`] unless it carries a challenge marker.
async fn fetch_engine(
    client: &reqwest::Client,
    engine: Engine_,
    query: &str,
    max_results: usize,
) -> SearchOutcome {
    let request = match engine {
        Engine_::DuckDuckGo => client
            .post("https://html.duckduckgo.com/html/")
            .form(&[("q", query)]),
        Engine_::Bing => client
            .get("https://www.bing.com/search")
            .query(&[("q", query)]),
    };
    let response = match request.header("User-Agent", BROWSER_USER_AGENT).send().await {
        Ok(response) => response,
        Err(_) => return SearchOutcome::Blocked,
    };
    if !response.status().is_success() {
        return SearchOutcome::Blocked;
    }
    let html = match response.text().await {
        Ok(html) => html,
        Err(_) => return SearchOutcome::Blocked,
    };
    classify(engine, &html, max_results)
}

/// Parses a results page and classifies it. Shared by [`fetch_engine`] and the
/// unit tests so the empty/blocked distinction is exercised directly.
fn classify(engine: Engine_, html: &str, max_results: usize) -> SearchOutcome {
    let (results, challenged) = match engine {
        Engine_::DuckDuckGo => (
            parse_duckduckgo_html(html, max_results),
            // DuckDuckGo serves its bot challenge as a 202 (or even a 200)
            // whose body loads `anomaly.js` and carries no results.
            html.contains("anomaly.js"),
        ),
        Engine_::Bing => (
            parse_bing_html(html, max_results),
            html.contains("sorry your network") || html.contains("/sorry/"),
        ),
    };
    if !results.is_empty() {
        SearchOutcome::Results(results)
    } else if challenged {
        SearchOutcome::Blocked
    } else {
        SearchOutcome::Empty
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
        // Drop DuckDuckGo's sponsored results — they resolve to a
        // `duckduckgo.com/y.js?ad_domain=…` redirect, not a real page, and
        // are just noise for a model reading the results.
        .filter(|((url, _), _)| !is_ad_url(url))
        .take(max_results)
        .map(|((url, title), snippet)| SearchResult {
            title,
            url,
            snippet,
        })
        .collect()
}

/// Whether a resolved result URL is a DuckDuckGo ad/redirect rather than a
/// real destination.
fn is_ad_url(url: &str) -> bool {
    url.contains("/y.js") || url.contains("ad_domain=") || url.contains("ad_provider=")
}

/// Extracts results from Bing's HTML results page — the fallback when
/// DuckDuckGo is blocked or empty. Each organic result is a
/// `<li class="b_algo">` whose `<h2><a href=…>` carries the title and link and
/// whose first `<p>` carries the snippet. Loose regex over the markup, same
/// spirit as [`parse_duckduckgo_html`].
pub(crate) fn parse_bing_html(html: &str, max_results: usize) -> Vec<SearchResult> {
    let link_pattern =
        Regex::new(r#"(?s)<h2[^>]*>\s*<a[^>]+href="([^"]+)"[^>]*>(.*?)</a>"#).expect("valid regex");
    let snippet_pattern = Regex::new(r#"(?s)<p[^>]*class="[^"]*b_lineclamp[^"]*"[^>]*>(.*?)</p>"#)
        .expect("valid regex");

    // Split on result items so a title and its snippet stay paired even when a
    // result has no snippet (a shared zip would drift out of alignment).
    html.split("<li class=\"b_algo\"")
        .skip(1)
        .filter_map(|block| {
            let caps = link_pattern.captures(block)?;
            let url = resolve_bing_url(&decode_html_entities(&caps[1]));
            // Skip Bing's own navigational and related-search links.
            if url.contains("bing.com") || url.contains("microsofttranslator") {
                return None;
            }
            let title = strip_tags(decode_html_entities(&caps[2]));
            if title.is_empty() {
                return None;
            }
            let snippet = snippet_pattern
                .captures(block)
                .map(|caps| strip_tags(decode_html_entities(&caps[1])))
                .unwrap_or_default();
            Some(SearchResult {
                title,
                url,
                snippet,
            })
        })
        .take(max_results)
        .collect()
}

/// Unwraps Bing's `bing.com/ck/a?…&u=a1<base64url>&…` click-tracking redirect
/// to the real destination, leaving direct links untouched. The `u` parameter
/// is the destination URL as url-safe base64 with an `a1` scheme prefix.
fn resolve_bing_url(href: &str) -> String {
    let Some(start) = href.find("u=a1") else {
        return href.to_string();
    };
    let encoded = &href[start + 4..];
    let encoded = encoded.split('&').next().unwrap_or(encoded);
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(encoded.trim_end_matches('='))
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .filter(|decoded| decoded.starts_with("http"))
        .unwrap_or_else(|| href.to_string())
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
