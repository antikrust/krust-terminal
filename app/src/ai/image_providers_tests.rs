use super::*;

#[test]
fn a_model_id_names_its_supplier_and_model() {
    let id = model_id(provider("together").expect("together"), "black-forest-labs/FLUX.1-schnell-Free");
    assert_eq!(id, "image/together/black-forest-labs/FLUX.1-schnell-Free");
    let (supplier, model) = parse_model_id(&id).expect("a listed model");
    assert_eq!(supplier.id, "together");
    assert!(model.free);
    assert!(parse_model_id("image/together/no-such-model").is_none());
    assert!(parse_model_id("api/openai/gpt-image-1").is_none());
    assert_eq!(label(&id), "Together AI · FLUX.1 schnell (free)");
}

#[test]
fn each_supplier_gets_the_request_it_wants() {
    let openai = provider("openai").expect("openai");
    let body = request_body(openai, "gpt-image-1", "a knob", 1024, 512, true);
    assert_eq!(body["size"], "1024x512");
    assert_eq!(body["background"], "transparent");
    assert!(body.get("response_format").is_none());

    let together = provider("together").expect("together");
    let body = request_body(together, "black-forest-labs/FLUX.1-schnell-Free", "a knob", 768, 512, true);
    assert_eq!(body["width"], 768);
    assert_eq!(body["height"], 512);
    assert_eq!(body["response_format"], "b64_json");
    assert!(body.get("size").is_none());
    assert!(body.get("background").is_none());

    let xai = provider("xai").expect("xai");
    let body = request_body(xai, "grok-2-image", "a knob", 1024, 1024, false);
    assert!(body.get("size").is_none());
    assert_eq!(body["response_format"], "b64_json");
}

#[test]
fn a_row_moves_one_place_and_stops_at_the_ends() {
    let rows: Vec<String> = ["a", "b", "c"].iter().map(|row| row.to_string()).collect();
    assert_eq!(moved(&rows, 1, -1), ["b", "a", "c"]);
    assert_eq!(moved(&rows, 1, 1), ["a", "c", "b"]);
    assert_eq!(moved(&rows, 0, -1), rows);
    assert_eq!(moved(&rows, 2, 1), rows);
    assert_eq!(moved(&rows, 9, 1), rows);
}
