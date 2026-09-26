//! Golden tests against what the reference app produced (tests/reference/*, written by
//! script/revendor from `assets:precompile` and the real Rails helpers).

use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/tests/reference/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

fn json_fixture(name: &str) -> Value {
    serde_json::from_str(&fixture(name)).unwrap()
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn get(path: &str) -> campfire_assets::StaticResponse {
    campfire_assets::serve(&campfire_assets::StaticRequest {
        method: "GET",
        path,
        ..Default::default()
    })
    .unwrap_or_else(|| panic!("{path} isn't served"))
}

#[test]
fn manifest_matches_the_reference_precompile() {
    let reference: BTreeMap<String, String> = json_fixture("manifest.json")
        .as_object()
        .unwrap()
        .iter()
        .map(|(logical, entry)| {
            (
                logical.clone(),
                entry["digested_path"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    let ours: BTreeMap<String, String> = campfire_assets::manifest()
        .iter()
        .map(|(l, d)| (l.to_string(), d.to_string()))
        .collect();

    let missing: Vec<_> = reference
        .iter()
        .filter(|(l, d)| ours.get(*l) != Some(d))
        .collect();
    let extra: Vec<_> = ours
        .keys()
        .filter(|l| !reference.contains_key(*l))
        .collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "differs from reference: {missing:?}, extra: {extra:?}"
    );

    let served: Value = serde_json::from_str(campfire_assets::manifest_json()).unwrap();
    let served_as_map: BTreeMap<_, _> = served.as_object().unwrap().iter().collect();
    let reference_json = json_fixture("manifest.json");
    assert_eq!(
        served_as_map,
        reference_json.as_object().unwrap().iter().collect()
    );
}

#[test]
fn compiled_files_are_byte_identical_to_the_reference_precompile() {
    let reference = json_fixture("compiled_sha256.json");
    let mut mismatched = Vec::new();
    for (digested_path, expected) in reference.as_object().unwrap() {
        let response = get(&format!("/assets/{digested_path}"));
        if sha256(&response.body) != expected.as_str().unwrap() {
            mismatched.push(digested_path.clone());
        }
    }
    assert!(
        mismatched.is_empty(),
        "compiled output differs for {mismatched:?}"
    );
    assert_eq!(
        reference.as_object().unwrap().len(),
        campfire_assets::manifest().len()
    );
}

#[test]
fn stylesheet_link_tag_all_matches_the_reference() {
    let tags = campfire_assets::stylesheet_link_tag_all(&[("data-turbo-track", "reload")]);
    assert_eq!(tags.html, fixture("stylesheet_link_tag_all.html"));
    assert_eq!(
        campfire_assets::append_preload_links("", &tags.preload_links),
        fixture("link_header.txt")
    );
}

#[test]
fn javascript_importmap_tags_match_the_reference() {
    assert_eq!(
        campfire_assets::javascript_importmap_tags(),
        fixture("javascript_importmap_tags.html")
    );
}

#[test]
fn public_files_are_served_like_action_dispatch_static() {
    for case in json_fixture("static_responses.json").as_array().unwrap() {
        let env = &case["env"];
        let request = campfire_assets::StaticRequest {
            method: case["method"].as_str().unwrap(),
            path: case["path"].as_str().unwrap(),
            range: env["HTTP_RANGE"].as_str(),
            accept_encoding: env["HTTP_ACCEPT_ENCODING"].as_str(),
            if_modified_since: None,
        };
        let label = format!("{} {} {env}", request.method, request.path);
        let expected_status = case["status"].as_u64().unwrap();
        let expected_headers = case["headers"].as_object().unwrap();

        // The probe's fallthrough app answers 404 with x-cascade: pass.
        if expected_status == 404 && expected_headers.get("x-cascade").is_some() {
            assert!(
                campfire_assets::serve(&request).is_none(),
                "{label} should fall through"
            );
            continue;
        }

        let response =
            campfire_assets::serve(&request).unwrap_or_else(|| panic!("{label} not served"));
        assert_eq!(response.status as u64, expected_status, "{label}");

        let ours: BTreeMap<String, String> = response
            .headers
            .iter()
            .filter(|(name, _)| *name != "last-modified")
            .map(|(name, value)| (name.to_string(), value.clone()))
            .collect();
        let theirs: BTreeMap<String, String> = expected_headers
            .iter()
            .map(|(name, value)| (name.clone(), value.as_str().unwrap().to_string()))
            .collect();

        // Our manifest lists the same entries in load-path order rather than the build
        // machine's readdir order, so its length and bytes can't match.
        if request.path == "/assets/.manifest.json" {
            assert_eq!(
                ours.get("content-type"),
                theirs.get("content-type"),
                "{label}"
            );
            continue;
        }

        assert_eq!(ours, theirs, "{label}");
        if request.method == "GET" {
            assert_eq!(
                sha256(&response.body),
                case["body_sha256"].as_str().unwrap(),
                "{label}"
            );
        }
    }
}

#[test]
fn last_modified_round_trips_to_a_304() {
    let response = get("/robots.txt");
    let last_modified = response.header("last-modified").unwrap().to_string();
    let not_modified = campfire_assets::serve(&campfire_assets::StaticRequest {
        method: "GET",
        path: "/robots.txt",
        if_modified_since: Some(&last_modified),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(not_modified.status, 304);
    assert!(not_modified.headers.is_empty() && not_modified.body.is_empty());
}

#[test]
fn head_requests_have_no_body() {
    let response = campfire_assets::serve(&campfire_assets::StaticRequest {
        method: "HEAD",
        path: "/robots.txt",
        ..Default::default()
    })
    .unwrap();
    assert_eq!(response.status, 200);
    assert!(response.body.is_empty());
    assert_eq!(response.header("content-length"), Some("99"));
}

#[test]
fn multiple_ranges_are_multipart() {
    let sound = campfire_assets::audio_path("56k.mp3");
    let response = campfire_assets::serve(&campfire_assets::StaticRequest {
        method: "GET",
        path: &sound,
        range: Some("bytes=0-1, 4-5"),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(response.status, 206);
    // Rack sets multipart/byteranges, then Static overwrites it with the file's type.
    assert_eq!(response.header("content-type"), Some("audio/mpeg"));
    let body = String::from_utf8_lossy(&response.body);
    assert!(
        body.starts_with("\r\n--AaB03x\r\ncontent-type: audio/mpeg\r\ncontent-range: bytes 0-1/")
    );
    assert!(body.ends_with("\r\n--AaB03x--\r\n"));
}
