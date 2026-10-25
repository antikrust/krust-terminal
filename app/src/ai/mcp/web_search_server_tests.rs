use super::{
    classify, format_results_as_text, keep_relevant, parse_brave_html, parse_duckduckgo_html,
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

const BRAVE_HTML: &str = r#"
<div class="snippet svelte-x" data-pos="0" data-type="web" data-keynav="true"><div class="result-body"><a href="https://en.wikipedia.org/wiki/Kris_Weston" target="_self" class="l1"><div class="site-name">Wikipedia</div> <div class="title search-snippet-title line-clamp-1 svelte-y" title="Kris Weston - Wikipedia">Kris Weston - Wikipedia</div></a><div class="generic-snippet"><div class="content desktop-default-regular t-primary line-clamp-dynamic svelte-z">Kristian &quot;Kris&quot; Weston (born 1972) is a British <strong>electronic</strong> musician.</div></div></div></div>
<div class="snippet svelte-x" data-pos="1" data-type="web" data-keynav="true"><div class="result-body"><a href="https://www.discogs.com/artist/34584-Kris-Weston" target="_self" class="l1"><div class="title search-snippet-title line-clamp-1 svelte-y" title="Kris Weston Discography | Discogs">Kris Weston Discography | Discogs</div></a></div></div>
"#;

#[test]
fn parses_brave_results() {
    let results = parse_brave_html(BRAVE_HTML, 5);
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].title, "Kris Weston - Wikipedia");
    assert_eq!(results[0].url, "https://en.wikipedia.org/wiki/Kris_Weston");
    assert_eq!(
        results[0].snippet,
        "Kristian \"Kris\" Weston (born 1972) is a British electronic musician."
    );
    // A result without a description still parses.
    assert_eq!(results[1].url, "https://www.discogs.com/artist/34584-Kris-Weston");
    assert_eq!(results[1].snippet, "");
}

fn result(title: &str, url: &str, snippet: &str) -> SearchResult {
    SearchResult {
        title: title.to_string(),
        url: url.to_string(),
        snippet: snippet.to_string(),
    }
}

#[test]
fn junk_results_that_never_mention_the_query_are_dropped() {
    let results = vec![
        result("Facebook – log in or sign up", "https://www.facebook.com/", "Log in"),
        result("Kris Weston - Wikipedia", "https://en.wikipedia.org/wiki/Kris_Weston", ""),
        result("Thrash", "https://example.com/weston-interview", "An interview"),
    ];
    let kept = keep_relevant("kris weston", results);
    let urls: Vec<&str> = kept.iter().map(|result| result.url.as_str()).collect();
    assert_eq!(
        urls,
        vec!["https://en.wikipedia.org/wiki/Kris_Weston", "https://example.com/weston-interview"]
    );
    // A query made only of common words can't judge relevance, so it keeps
    // everything.
    assert_eq!(keep_relevant("what is the", vec![result("Anything", "https://x.y/", "")]).len(), 1);
}

#[test]
fn classify_distinguishes_results_empty_and_blocked() {
    assert!(matches!(
        classify(Engine_::Brave, BRAVE_HTML, "kris weston", 5),
        SearchOutcome::Results(_)
    ));
    // Results about something else entirely count as none.
    assert!(matches!(
        classify(Engine_::Brave, BRAVE_HTML, "volcano eruptions", 5),
        SearchOutcome::Empty
    ));
    assert!(matches!(
        classify(Engine_::DuckDuckGo, "<html><body>no results</body></html>", "rust", 5),
        SearchOutcome::Empty
    ));
    // DuckDuckGo's bot challenge carries anomaly.js and no results.
    assert!(matches!(
        classify(
            Engine_::DuckDuckGo,
            "<html><script src=\"anomaly.js\"></script></html>",
            "rust",
            5
        ),
        SearchOutcome::Blocked
    ));
}
