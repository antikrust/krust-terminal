use super::{
    api_model_parts, model_id_for, model_price, parse_live_free_models, provider, PROVIDERS,
};

#[test]
fn model_ids_round_trip_for_every_listed_model() {
    for provider in PROVIDERS {
        for model in provider.models {
            let id = model_id_for(provider, model.id);
            let (parsed_provider, parsed_model) =
                api_model_parts(&id).unwrap_or_else(|| panic!("{id} should parse"));
            assert_eq!(parsed_provider.id, provider.id);
            assert_eq!(parsed_model, model.id);
        }
    }
}

#[test]
fn model_part_may_contain_slashes() {
    let (provider, model) = api_model_parts("api/groq/openai/gpt-oss-120b").expect("parses");
    assert_eq!(provider.id, "groq");
    assert_eq!(model, "openai/gpt-oss-120b");
}

#[test]
fn rejects_ids_that_are_not_api_models() {
    assert!(api_model_parts("ollama/qwen3:14b").is_none());
    assert!(api_model_parts("claude-opus-4-8").is_none());
    // Unknown provider, or a provider with no model.
    assert!(api_model_parts("api/nope/some-model").is_none());
    assert!(api_model_parts("api/zai/").is_none());
    assert!(api_model_parts("api/zai").is_none());
}

#[test]
fn zai_is_listed_with_its_free_models() {
    let zai = provider("zai").expect("Z.ai is a provider");
    assert_eq!(zai.base_url, "https://api.z.ai/api/paas/v4");
    let models: Vec<&str> = zai.models.iter().map(|model| model.id).collect();
    assert_eq!(models, vec!["glm-4.7-flash", "glm-4.5-flash"]);
}

#[test]
fn every_provider_links_to_an_https_key_page_and_lists_models() {
    for provider in PROVIDERS {
        assert!(
            provider.key_url.starts_with("https://"),
            "{} key link: {}",
            provider.id,
            provider.key_url
        );
        assert!(provider.base_url.starts_with("https://"), "{}", provider.id);
        assert!(!provider.models.is_empty(), "{} has no models", provider.id);
    }
}

#[test]
fn live_free_list_keeps_free_models_curated_first() {
    let openrouter = provider("openrouter").expect("OpenRouter is a provider");
    let body = r#"{"data":[
        {"id":"acme/big-model:free","name":"Acme: Big Model (free)","context_length":1000000},
        {"id":"acme/paid-model","name":"Acme: Paid Model","context_length":2000000},
        {"id":"google/gemma-4-31b-it:free","name":"Google: Gemma 4 31B (free)","context_length":262144},
        {"id":"nvidia/nemotron-3-super-120b-a12b:free","name":"NVIDIA: Nemotron 3 Super (free)","context_length":1000000}
    ]}"#;
    let models = parse_live_free_models(body, openrouter);
    let ids: Vec<&str> = models.iter().map(|(id, _)| id.as_str()).collect();
    // Paid models are dropped; curated ones lead in curated order, then the
    // rest by context window.
    assert_eq!(
        ids,
        vec![
            "nvidia/nemotron-3-super-120b-a12b:free",
            "google/gemma-4-31b-it:free",
            "acme/big-model:free",
        ]
    );
    // Curated names are kept; others lose the "(free)" suffix.
    assert_eq!(models[0].1, "Nemotron 3 Super 120B (NVIDIA)");
    assert_eq!(models[2].1, "Acme: Big Model");
    // An unreadable body yields nothing, so the curated list stays.
    assert!(parse_live_free_models("not json", openrouter).is_empty());
}

#[test]
fn prices_cover_listed_models_and_free_live_ones() {
    let venice = provider("venice").expect("Venice is a provider");
    assert_eq!(model_price(venice, "deepseek-v4-flash-0731"), Some((0.13, 0.26)));
    let zai = provider("zai").expect("Z.ai is a provider");
    assert_eq!(model_price(zai, "glm-4.7-flash"), Some((0.0, 0.0)));
    // A free model only in OpenRouter's live list is free; anything unknown
    // claims no price.
    let openrouter = provider("openrouter").expect("OpenRouter is a provider");
    assert_eq!(model_price(openrouter, "acme/new-model:free"), Some((0.0, 0.0)));
    assert_eq!(model_price(venice, "some-unlisted-model"), None);
}
