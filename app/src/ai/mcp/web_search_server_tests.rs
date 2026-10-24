use super::{
    classify, format_results_as_text, parse_bing_html, parse_duckduckgo_html, resolve_bing_url,
    resolve_result_url, Engine_, SearchOutcome, SearchResult,
};

const SAMPLE_HTML: &str = r#"
<div class="results_links results_links_deep web-result">
    <div class="links_main links_deep result__body">
        <h2 class="result__title">
            <a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fdocs%2Ekrust%2Edev%2Fguides%2Follama&amp;rut=abc">
                Set up <b>Ollama</b> | Krust
            </a>
        </h2>
        <a class="result__snippet" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fdocs%2Ekrust%2Edev%2Fguides%2Follama">
            Install <b>Ollama</b>, run LLMs locally, and integrate them into Krust.
        </a>
    </div>
</div>
<div class="results_links results_links_deep web-result">
    <div class="links_main links_deep result__body">
        <h2 class="result__title">
            <a rel="nofollow" class="result__a" href="https://example.com/direct-link">
                Direct link, no redirect
            </a>
        </h2>
        <a class="result__snippet">A result whose href wasn't wrapped in a DDG redirect.</a>
    </div>
</div>
"#;

const AD_HTML: &str = r#"
<div class="results_links web-result">
    <h2 class="result__title">
        <a class="result__a" href="//duckduckgo.com/y.js?ad_domain=amazon.co.uk&amp;ad_provider=bingv7aa">
            Buy Rust Books - Amazon
        </a>
    </h2>
    <a class="result__snippet">Sponsored</a>
</div>
<div class="results_links web-result">
    <h2 class="result__title">
        <a class="result__a" href="https://doc.rust-lang.org/">
            The Rust Programming Language
        </a>
    </h2>
    <a class="result__snippet">Official docs.</a>
</div>
"#;

#[test]
fn drops_duckduckgo_sponsored_results() {
    let results = parse_duckduckgo_html(AD_HTML, 5);
    assert_eq!(
        results.len(),
        1,
        "the sponsored y.js result should be filtered"
    );
    assert_eq!(results[0].url, "https://doc.rust-lang.org/");
}

#[test]
fn parses_titles_snippets_and_unwraps_ddg_redirect_urls() {
    let results = parse_duckduckgo_html(SAMPLE_HTML, 5);
    assert_eq!(results.len(), 2);

    assert_eq!(results[0].title, "Set up Ollama | Krust");
    // Matches the `uddg=` payload in SAMPLE_HTML once decoded.
    assert_eq!(results[0].url, "https://docs.krust.dev/guides/ollama");
    assert_eq!(
        results[0].snippet,
        "Install Ollama, run LLMs locally, and integrate them into Krust."
    );

    assert_eq!(results[1].title, "Direct link, no redirect");
    assert_eq!(results[1].url, "https://example.com/direct-link");
}

#[test]
fn respects_max_results() {
    let results = parse_duckduckgo_html(SAMPLE_HTML, 1);
    assert_eq!(results.len(), 1);
}

#[test]
fn empty_html_yields_no_results() {
    assert!(parse_duckduckgo_html("<html></html>", 5).is_empty());
}

#[test]
fn resolves_ddg_redirect_and_leaves_direct_urls_alone() {
    assert_eq!(
        resolve_result_url("//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fpage&rut=xyz"),
        "https://example.com/page"
    );
    assert_eq!(
        resolve_result_url("https://example.com/direct"),
        "https://example.com/direct"
    );
}

#[test]
fn formats_results_as_readable_text() {
    let results = vec![SearchResult {
        title: "Title".to_string(),
        url: "https://example.com".to_string(),
        snippet: "Snippet.".to_string(),
    }];
    let text = format_results_as_text("my query", &results);
    assert!(text.contains("Search results for \"my query\":"));
    assert!(text.contains("1. Title"));
    assert!(text.contains("https://example.com"));
    assert!(text.contains("Snippet."));
}

#[test]
fn formats_no_results_message() {
    assert_eq!(
        format_results_as_text("nothing here", &[]),
        "No results found for \"nothing here\"."
    );
}

const BING_HTML: &str = r#"
<ol id="b_results">
  <li class="b_algo"><h2><a href="https://www.bing.com/ck/a?!&amp;&amp;u=a1aHR0cHM6Ly90b2tpby5ycy8&amp;ntb=1">Tokio — Rust runtime</a></h2>
    <p class="b_lineclamp2">An asynchronous <strong>runtime</strong> for Rust.</p></li>
  <li class="b_algo"><h2><a href="https://doc.rust-lang.org/">The Rust Programming Language</a></h2>
    <p class="b_lineclamp3">Official docs.</p></li>
  <li class="b_algo"><h2><a href="https://www.bing.com/search?q=rust">rust - Bing</a></h2>
    <p class="b_lineclamp2">A navigational Bing link that should be dropped.</p></li>
</ol>
"#;

#[test]
fn parses_bing_results_and_decodes_ck_redirect() {
    let results = parse_bing_html(BING_HTML, 5);
    // The bing.com navigational link is dropped; the ck/a redirect is decoded.
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].title, "Tokio — Rust runtime");
    assert_eq!(results[0].url, "https://tokio.rs/");
    assert_eq!(results[0].snippet, "An asynchronous runtime for Rust.");
    assert_eq!(results[1].url, "https://doc.rust-lang.org/");
}

#[test]
fn resolves_bing_ck_redirect_and_leaves_direct_urls_alone() {
    assert_eq!(
        resolve_bing_url("https://www.bing.com/ck/a?!&&u=a1aHR0cHM6Ly90b2tpby5ycy8&ntb=1"),
        "https://tokio.rs/"
    );
    assert_eq!(
        resolve_bing_url("https://doc.rust-lang.org/"),
        "https://doc.rust-lang.org/"
    );
}

#[test]
fn classify_distinguishes_results_empty_and_blocked() {
    // A page that parses to results.
    assert!(matches!(
        classify(Engine_::Bing, BING_HTML, 5),
        SearchOutcome::Results(_)
    ));
    // A real page with no result markers is empty, not blocked.
    assert!(matches!(
        classify(Engine_::DuckDuckGo, "<html><body>no results</body></html>", 5),
        SearchOutcome::Empty
    ));
    // DuckDuckGo's bot challenge carries anomaly.js and no results.
    assert!(matches!(
        classify(
            Engine_::DuckDuckGo,
            "<html><script src=\"anomaly.js\"></script></html>",
            5
        ),
        SearchOutcome::Blocked
    ));
}
