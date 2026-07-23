use super::*;

#[test]
fn qualifying_round_trips_and_leaves_local_models_alone() {
    let qualified = qualify("qwen2.5-coder:32b", "http://192.168.1.150:11434");
    assert_eq!(
        split(&qualified),
        ("qwen2.5-coder:32b", Some("http://192.168.1.150:11434"))
    );

    // An unqualified name is a local model, and must survive untouched so
    // ids already written into settings keep resolving.
    assert_eq!(split("llama3.1:8b"), ("llama3.1:8b", None));
    assert_eq!(qualify("llama3.1:8b", ""), "llama3.1:8b");
}

#[test]
fn model_names_containing_slashes_and_colons_still_split() {
    let qualified = qualify("hf.co/user/model:latest", "http://pc:11434");
    assert_eq!(
        split(&qualified),
        ("hf.co/user/model:latest", Some("http://pc:11434"))
    );
}

#[test]
fn base_url_falls_back_to_the_local_install() {
    let local = "http://127.0.0.1:11434";
    assert_eq!(base_url_for("llama3.1:8b", local), local);
    assert_eq!(
        base_url_for("llama3.1:8b@@http://pc:11434", local),
        "http://pc:11434"
    );
}

#[test]
fn host_input_accepts_what_people_actually_type() {
    for input in [
        "192.168.1.150",
        " 192.168.1.150 ",
        "192.168.1.150:11434",
        "http://192.168.1.150:11434",
        "http://192.168.1.150:11434/",
    ] {
        assert_eq!(
            normalize_host_input(input).as_deref(),
            Ok("http://192.168.1.150:11434"),
            "input {input:?}"
        );
    }
    assert_eq!(
        normalize_host_input("bigbox:1234").as_deref(),
        Ok("http://bigbox:1234")
    );
}

#[test]
fn host_input_rejects_what_would_fail_later() {
    assert!(normalize_host_input("").is_err());
    assert!(normalize_host_input("   ").is_err());
    // A path would be silently dropped once endpoints are appended.
    assert!(normalize_host_input("192.168.1.150:11434/api").is_err());
    assert!(normalize_host_input("192.168.1.150:not-a-port").is_err());
    assert!(normalize_host_input("ftp://192.168.1.150").is_err());
    assert!(normalize_host_input("192.168.1.150:99999").is_err());
    assert!(normalize_host_input("has spaces").is_err());
}

#[test]
fn hosts_round_trip_through_the_setting_and_bad_json_is_survivable() {
    let hosts = vec![RemoteHost {
        url: "http://192.168.1.150:11434".to_owned(),
        models: vec!["qwen2.5-coder:32b".to_owned(), "devstral:latest".to_owned()],
        label: Some("the PC".to_owned()),
    }];
    let json = to_json(&hosts);
    assert_eq!(parse_hosts(Some(&json)), hosts);

    // The setting is a cache; losing it must degrade to "no remotes".
    assert!(parse_hosts(None).is_empty());
    assert!(parse_hosts(Some("")).is_empty());
    assert!(parse_hosts(Some("{not json")).is_empty());
}

#[test]
fn qualified_models_and_display_names_read_well() {
    let hosts = vec![RemoteHost {
        url: "http://192.168.1.150:11434".to_owned(),
        models: vec!["qwen2.5-coder:32b".to_owned()],
        label: Some("the PC".to_owned()),
    }];
    let qualified = qualified_models(&hosts);
    assert_eq!(
        qualified,
        vec!["qwen2.5-coder:32b@@http://192.168.1.150:11434"]
    );
    assert_eq!(
        display_name(&qualified[0], &hosts),
        "qwen2.5-coder:32b (the PC)"
    );
    assert_eq!(display_name("llama3.1:8b", &hosts), "llama3.1:8b");

    // A model whose host is no longer configured still names its address
    // rather than rendering the raw separator.
    assert_eq!(
        display_name("m:1b@@http://192.168.1.99:11434", &hosts),
        "m:1b (192.168.1.99:11434)"
    );
}

#[test]
fn a_host_with_no_label_displays_its_address() {
    let host = RemoteHost {
        url: "http://192.168.1.150:11434".to_owned(),
        models: vec![],
        label: None,
    };
    assert_eq!(host.display(), "192.168.1.150:11434");
}
