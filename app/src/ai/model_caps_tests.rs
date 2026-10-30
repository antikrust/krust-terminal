use super::*;

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| item.to_string()).collect()
}

#[test]
fn labels_name_capabilities_and_unknown_ones_are_ignored() {
    assert_eq!(Caps::from_label("vision"), Some(Caps::VISION));
    assert_eq!(Caps::from_label(" Images "), Some(Caps::VISION));
    assert_eq!(
        Caps::from_label("image_out"),
        Some(Caps {
            image_out: true,
            ..Caps::NONE
        })
    );
    assert_eq!(Caps::from_label("tools"), None);
    assert_eq!(
        Caps::from_labels(["vision", "embed", "nonsense"]),
        Caps {
            vision: true,
            embed: true,
            image_out: false,
        }
    );
    assert_eq!(Caps::from_labels([]), Caps::NONE);
}

#[test]
fn labels_round_trip() {
    let caps = Caps {
        vision: true,
        image_out: true,
        embed: false,
    };
    assert_eq!(Caps::from_labels(caps.labels()), caps);
    assert!(Caps::NONE.labels().is_empty());
}

#[test]
fn covers_means_every_need_is_met() {
    let reads_and_embeds = Caps {
        vision: true,
        embed: true,
        image_out: false,
    };
    assert!(reads_and_embeds.covers(Caps::VISION));
    assert!(reads_and_embeds.covers(reads_and_embeds));
    assert!(reads_and_embeds.covers(Caps::NONE));
    assert!(!Caps::VISION.covers(reads_and_embeds));
    assert!(!Caps::NONE.covers(Caps::VISION));
    assert!(Caps::NONE.covers(Caps::NONE));
}

#[test]
fn describe_says_what_the_caps_are_in_words() {
    assert_eq!(Caps::VISION.describe(), "reads images");
    assert_eq!(
        Caps {
            vision: true,
            image_out: true,
            embed: false,
        }
        .describe(),
        "reads images and makes images"
    );
    assert!(unmet_message(Caps::VISION).contains("reads images"));
}

#[test]
fn override_reads_forced_on_and_forced_off() {
    let entries = strings(&[
        "ollama/qwen3.5:4b=vision",
        "api/openrouter/foo=-vision,embed",
        "ollama/other=vision",
    ]);
    assert_eq!(
        override_for(&entries, "ollama/qwen3.5:4b"),
        (Caps::VISION, Caps::NONE)
    );
    assert_eq!(
        override_for(&entries, "api/openrouter/foo"),
        (
            Caps {
                embed: true,
                ..Caps::NONE
            },
            Caps::VISION
        )
    );
    assert_eq!(
        override_for(&entries, "ollama/unlisted"),
        (Caps::NONE, Caps::NONE)
    );
}

#[test]
fn override_ignores_garbage() {
    let entries = strings(&["no equals sign", "ollama/x=,bogus,-bogus", "=vision"]);
    assert_eq!(override_for(&entries, "ollama/x"), (Caps::NONE, Caps::NONE));
}

#[test]
fn flipping_a_model_that_lacks_vision_forces_it_on() {
    let flipped = flipped_override(&[], "ollama/m", Caps::VISION, Caps::NONE);
    assert_eq!(flipped, strings(&["ollama/m=vision"]));
}

#[test]
fn flipping_a_model_that_has_vision_forces_it_off() {
    let flipped = flipped_override(&[], "api/p/m", Caps::VISION, Caps::VISION);
    assert_eq!(flipped, strings(&["api/p/m=-vision"]));
}

#[test]
fn flipping_back_drops_the_override() {
    let forced_on = strings(&["ollama/m=vision", "ollama/keep=vision"]);
    let flipped = flipped_override(&forced_on, "ollama/m", Caps::VISION, Caps::NONE);
    assert_eq!(flipped, strings(&["ollama/keep=vision"]));

    let forced_off = strings(&["api/p/m=-vision"]);
    let flipped = flipped_override(&forced_off, "api/p/m", Caps::VISION, Caps::VISION);
    assert!(flipped.is_empty());
}

#[test]
fn flipping_one_flag_leaves_the_others() {
    let entries = strings(&["ollama/m=embed"]);
    let flipped = flipped_override(&entries, "ollama/m", Caps::VISION, Caps::NONE);
    assert_eq!(flipped, strings(&["ollama/m=vision,embed"]));
}

#[test]
fn ollama_capabilities_list_is_read() {
    let caps = from_ollama_capabilities(&strings(&["completion", "vision", "tools"]));
    assert_eq!(caps, Caps::VISION);
    let embedder = from_ollama_capabilities(&strings(&["embedding"]));
    assert_eq!(
        embedder,
        Caps {
            embed: true,
            ..Caps::NONE
        }
    );
    assert_eq!(
        from_ollama_capabilities(&strings(&["completion", "tools"])),
        Caps::NONE
    );
    // Ollama's image models (x/flux2-klein, x/z-image-turbo) list "image".
    assert_eq!(
        from_ollama_capabilities(&strings(&["image"])),
        Caps {
            image_out: true,
            ..Caps::NONE
        }
    );
}

#[test]
fn modalities_are_read() {
    let caps = from_modalities(&strings(&["text", "image"]), &strings(&["text"]));
    assert_eq!(caps, Caps::VISION);
    let maker = from_modalities(&strings(&["text"]), &strings(&["image", "text"]));
    assert_eq!(
        maker,
        Caps {
            image_out: true,
            ..Caps::NONE
        }
    );
    assert_eq!(from_modalities(&[], &[]), Caps::NONE);
}

#[test]
fn name_guess_only_ever_says_yes_for_known_families() {
    assert_eq!(caps_from_name("claude-sonnet-5-5"), Caps::VISION);
    assert_eq!(caps_from_name("google/gemini-2.5-flash:free"), Caps::VISION);
    assert_eq!(caps_from_name("qwen/qwen2.5-vl-72b-instruct"), Caps::VISION);
    assert_eq!(caps_from_name("deepseek-chat"), Caps::NONE);
    assert_eq!(caps_from_name("llama-3.3-70b-instruct"), Caps::NONE);
}

#[test]
fn a_local_agent_app_reads_image_files_itself() {
    assert_eq!(base_caps("cli/claude/default"), Caps::VISION);
    assert_eq!(base_caps("cli/codex/gpt-5"), Caps::VISION);
}
