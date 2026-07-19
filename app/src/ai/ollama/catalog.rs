//! The model catalog, scraped from <https://ollama.com/library>.
//!
//! Ollama publishes no API for listing its library — only per-model registry
//! lookups — so this parses the listing page's HTML. That is a contract nobody
//! promised us, which drives two decisions:
//!
//! 1. Parsing is deliberately loose. It keys off `href="/library/<name>"` and
//!    the `<span>` contents inside each entry, not on class names or nesting,
//!    because those are Tailwind utility strings that change with any redesign.
//! 2. There is a [`bundled`] fallback list. A markup change should degrade the
//!    page to a slightly stale catalog, never to an empty one.
//!
//! If [`parse_library_html`] starts returning nothing, the markup moved — see
//! `test_data/library_sample.html` for the shape it was written against.

use super::capability::ParameterSize;

/// One advertised size of a model.
///
/// Keeps the library's literal tag text alongside the parsed parameter count.
/// Reconstructing the tag from the number is lossy — Ollama writes sub-billion
/// sizes as `0.5b`, not `500m` — and a wrong tag means a failed pull.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelSize {
    /// Exactly as the library wrote it: `7b`, `0.5b`, `1.5b`.
    pub tag: String,
    pub params: ParameterSize,
}

/// One model family from the library listing.
#[derive(Debug, Clone, PartialEq)]
pub struct CatalogEntry {
    /// Library name, e.g. `qwen2.5-coder`.
    pub name: String,
    pub description: String,
    /// Capability tags such as `tools`, `vision`, `thinking`, `embedding`.
    pub capabilities: Vec<String>,
    /// Advertised sizes, smallest first.
    pub sizes: Vec<ModelSize>,
}

impl CatalogEntry {
    /// The tag to pull for a given size, e.g. `qwen2.5-coder:7b`.
    pub fn tag_for(&self, size: &ModelSize) -> String {
        format!("{}:{}", self.name, size.tag)
    }

    /// Smallest advertised size — the one most likely to fit. Part of the
    /// public catalog API; exercised by tests even where the UI shows all
    /// sizes rather than only the smallest.
    #[allow(dead_code)]
    pub fn smallest_size(&self) -> Option<&ModelSize> {
        self.sizes.first()
    }

    /// Embedding models can't be chatted with; the page hides them by default.
    pub fn is_embedding_only(&self) -> bool {
        self.capabilities.iter().any(|c| c == "embedding")
    }
}

/// Parses the library listing page.
///
/// Returns entries in page order (Ollama sorts by popularity). An empty result
/// means the markup changed — callers should fall back to [`bundled`].
pub fn parse_library_html(html: &str) -> Vec<CatalogEntry> {
    let mut entries = Vec::new();

    // Split on the anchor that opens each model entry rather than on <li>,
    // whose attributes are pure styling and change freely.
    for chunk in html.split("href=\"/library/").skip(1) {
        let Some(name_end) = chunk.find('"') else {
            continue;
        };
        let name = chunk[..name_end].trim();
        // Listing links are bare names; anything with a slash or query is a
        // deeper link (tags, blobs) that shouldn't become a catalog entry.
        if name.is_empty() || name.contains('/') || name.contains('?') {
            continue;
        }
        if entries.iter().any(|e: &CatalogEntry| e.name == name) {
            continue;
        }

        // Bound the entry at the start of the next one so spans don't bleed
        // across models.
        let body = &chunk[name_end..];

        let description = first_paragraph(body).unwrap_or_default();
        let spans = span_texts(body);

        let mut capabilities = Vec::new();
        let mut sizes = Vec::new();
        for span in spans {
            if let Some(params) = ParameterSize::parse(&span) {
                let tag = span.to_ascii_lowercase();
                if !sizes.iter().any(|s: &ModelSize| s.tag == tag) {
                    sizes.push(ModelSize { tag, params });
                }
            } else if is_capability_tag(&span) {
                capabilities.push(span);
            }
        }
        sizes.sort_by(|a, b| a.params.billions.total_cmp(&b.params.billions));

        entries.push(CatalogEntry {
            name: name.to_string(),
            description,
            capabilities,
            sizes,
        });
    }

    entries
}

/// The capability chips Ollama shows. Matching a known set keeps stray spans
/// (pull counts, "Updated", relative dates) out of the capability list.
fn is_capability_tag(text: &str) -> bool {
    matches!(
        text,
        "tools" | "vision" | "thinking" | "embedding" | "cloud"
    )
}

fn first_paragraph(body: &str) -> Option<String> {
    let start = body.find("<p")?;
    let open_end = body[start..].find('>')? + start + 1;
    let end = body[open_end..].find("</p>")? + open_end;
    Some(decode_entities(strip_tags(&body[open_end..end]).trim()))
}

/// Text content of every `<span>`, in order.
fn span_texts(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(start) = rest.find("<span") {
        let Some(open_end) = rest[start..].find('>').map(|i| i + start + 1) else {
            break;
        };
        let Some(close) = rest[open_end..].find("</span>").map(|i| i + open_end) else {
            break;
        };
        let text = decode_entities(rest[open_end..close].trim());
        if !text.is_empty() {
            out.push(text);
        }
        rest = &rest[close + "</span>".len()..];
    }
    out
}

fn strip_tags(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut depth = 0usize;
    for c in input.chars() {
        match c {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

/// Minimal entity decoding — the listing only uses a handful.
fn decode_entities(input: &str) -> String {
    input
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .trim()
        .to_string()
}

/// Offline fallback, used when the fetch fails or the markup changes.
///
/// Small on purpose: enough to get someone running a local model today,
/// without pretending to mirror a library that changes weekly.
pub fn bundled() -> Vec<CatalogEntry> {
    fn entry(
        name: &str,
        description: &str,
        capabilities: &[&str],
        sizes: &[f64],
    ) -> CatalogEntry {
        CatalogEntry {
            name: name.to_string(),
            description: description.to_string(),
            capabilities: capabilities.iter().map(|c| c.to_string()).collect(),
            sizes: sizes
                .iter()
                .map(|b| ModelSize {
                    tag: if b.fract() == 0.0 {
                        format!("{b:.0}b")
                    } else {
                        format!("{b:.1}b")
                    },
                    params: ParameterSize { billions: *b },
                })
                .collect(),
        }
    }

    vec![
        entry(
            "qwen2.5-coder",
            "Code-specialised Qwen, strong at completion and review for its size.",
            &["tools"],
            &[0.5, 1.5, 3.0, 7.0, 14.0, 32.0],
        ),
        entry(
            "llama3.2",
            "Meta's compact general-purpose model.",
            &["tools"],
            &[1.0, 3.0],
        ),
        entry(
            "llama3.1",
            "Meta's general-purpose model with tool support.",
            &["tools"],
            &[8.0, 70.0, 405.0],
        ),
        entry(
            "deepseek-r1",
            "Reasoning model that shows its working.",
            &["tools", "thinking"],
            &[1.5, 7.0, 8.0, 14.0, 32.0, 70.0, 671.0],
        ),
        entry(
            "mistral",
            "Fast, well-rounded 7B model.",
            &["tools"],
            &[7.0],
        ),
        entry(
            "phi4-mini",
            "Microsoft's small model, unusually capable for its footprint.",
            &["tools"],
            &[3.8],
        ),
        entry(
            "gemma3",
            "Google's open model family.",
            &["vision"],
            &[1.0, 4.0, 12.0, 27.0],
        ),
        entry(
            "codellama",
            "Meta's code model. Used by the built-in local coding harness.",
            &[],
            &[7.0, 13.0, 34.0, 70.0],
        ),
        entry(
            "deepseek-coder",
            "Compact code model. Used by the built-in local coding harness.",
            &[],
            &[1.3, 6.7, 33.0],
        ),
    ]
}

pub const LIBRARY_URL: &str = "https://ollama.com/library";

#[cfg(test)]
#[path = "catalog_tests.rs"]
mod tests;
