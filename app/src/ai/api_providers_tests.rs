use super::{api_model_parts, model_id_for, provider, PROVIDERS};

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
