//! Frozen outputs from the unchanged Electron function with synthetic native input.
//! These tests are source-selection parity, NOT Windows enumeration/capture evidence.
use nimbo_desktop::sources::{NativeProcess, NativeWindow, game_name, missing_sources};
use serde_json::{Value, json};

fn run_case(name: &str) {
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/sources-electron.json")).unwrap();
    let case = fixture["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == name)
        .unwrap();
    let windows: Vec<NativeWindow> = case["windows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| NativeWindow {
            hwnd: w["hwnd"].as_str().unwrap().to_owned(),
            pid: w["pid"].as_u64().unwrap().try_into().unwrap(),
            title: w["title"].as_str().unwrap().to_owned(),
            path: w["path"].as_str().unwrap().to_owned(),
            class_name: w["class"].as_str().unwrap().to_owned(),
            visible: w["visible"].as_bool().unwrap(),
            owned: w["owned"].as_bool().unwrap(),
            tool: w["tool"].as_bool().unwrap(),
            cloaked: w["cloaked"].as_bool().unwrap(),
            minimized: w["minimized"].as_bool().unwrap(),
            fullscreen: w["fullscreen"].as_bool().unwrap(),
        })
        .collect();
    let processes: Vec<NativeProcess> = case["processes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| NativeProcess {
            pid: p["pid"].as_u64().unwrap().try_into().unwrap(),
            name: p["name"].as_str().unwrap().to_owned(),
        })
        .collect();
    let known: Vec<String> = case["known"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect();
    let own: Vec<u32> = case["own"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_u64().unwrap().try_into().unwrap())
        .collect();
    let failed_icons = case["iconFailures"].as_array().cloned().unwrap_or_default();
    let actual = missing_sources(&known, &windows, &processes, &own, |path| {
        if failed_icons.contains(&json!(path)) {
            String::new()
        } else {
            "data:native-icon".to_owned()
        }
    });
    let actual = Value::Array(actual.iter().map(|source| source.to_json()).collect());
    assert_eq!(actual, case["expected"], "Electron parity case: {name}");
}

// These additional cases characterize the now-working port; no historical RED is claimed.
#[test]
fn metadata_listing_is_complete_even_without_thumbnail_images() {
    run_case("complete-list-not-thumbnail-budget");
}

#[test]
fn no_sources_is_an_empty_list_not_a_fabricated_source() {
    run_case("empty");
}

#[test]
fn every_known_game_also_has_the_same_picker_metadata_as_electron() {
    run_case("all-game-aliases");
}

#[test]
fn fallback_names_keep_unicode_and_icon_failures_do_not_remove_sources() {
    run_case("fallback-names-icons-and-unicode");
}

#[test]
fn process_only_games_prefer_shipping_without_duplicating_shown_or_own_processes() {
    run_case("process-only-shipping-and-own-exclusion");
}

#[test]
fn unreal_and_visible_fullscreen_windows_follow_the_existing_exceptions() {
    run_case("unreal-and-fullscreen-fallback");
}

#[test]
fn hidden_games_use_the_best_window_and_ignore_input_method_helpers() {
    run_case("hidden-game-window-ranking");
}

#[test]
fn all_game_aliases_match_the_original_electron_table() {
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/sources-electron.json")).unwrap();
    for (exe, label) in fixture["games"].as_object().unwrap() {
        assert_eq!(game_name(exe), label.as_str(), "game alias {exe}");
        assert_eq!(
            game_name(&format!("{}.EXE", exe.to_uppercase())),
            label.as_str()
        );
    }
    for unknown in ["", "unknown", "cs2.exe.exe", "C:\\cs2.exe"] {
        assert_eq!(game_name(unknown), None);
    }
}

#[test]
fn windows_already_known_to_the_capturer_are_not_duplicated() {
    run_case("known-handles-and-known-processes");
}

#[test]
fn visible_windows_exclude_the_app_tree_and_system_auxiliaries() {
    run_case("visible-own-and-system-filters");
}
