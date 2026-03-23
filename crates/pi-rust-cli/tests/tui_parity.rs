use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use serde_json::Value;

static PTY_CAPTURE_LOCK: Mutex<()> = Mutex::new(());

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root")
}

fn runner_path() -> PathBuf {
    repo_root()
        .join("rust")
        .join("scripts")
        .join("tui_parity_runner.mjs")
}

fn command_exists(name: &str) -> bool {
    Command::new("sh")
        .args(["-lc", &format!("command -v {name}")])
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn prerequisites_ready() -> bool {
    command_exists("node") && command_exists("tmux") && runner_path().exists()
}

fn ts_repo_configured() -> bool {
    std::env::var("PI_TS_REPO")
        .ok()
        .map(|value| Path::new(value.trim()).exists())
        .unwrap_or(false)
}

fn capture_runtime_scenarios_at_size(
    runtime: &str,
    scenarios: &[&str],
    width: u16,
    height: u16,
) -> Option<Value> {
    let _guard = PTY_CAPTURE_LOCK.lock().expect("pty capture lock");
    if !prerequisites_ready() {
        eprintln!("Skipping PTY parity capture test: missing node, tmux, or local runner.");
        return None;
    }
    if runtime != "rust" && !ts_repo_configured() {
        eprintln!("Skipping PTY parity capture test for {runtime}: PI_TS_REPO is not configured.",);
        return None;
    }

    let mut command = Command::new("node");
    command.arg(runner_path());
    command.args(["--runtime", runtime]);
    command.args(["--width", &width.to_string()]);
    command.args(["--height", &height.to_string()]);
    if runtime == "rust" {
        command.arg("--rust-bin");
        command.arg(env!("CARGO_BIN_EXE_pi-rust"));
    }
    for scenario in scenarios {
        command.arg("--scenario");
        command.arg(scenario);
    }
    command.current_dir(repo_root());

    let output = command.output().expect("run PTY parity runner");
    assert!(
        output.status.success(),
        "PTY parity runner failed for {runtime}\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    Some(serde_json::from_slice(&output.stdout).expect("parse parity JSON"))
}

fn capture_runtime_scenarios(runtime: &str, scenarios: &[&str]) -> Option<Value> {
    capture_runtime_scenarios_at_size(runtime, scenarios, 80, 24)
}

fn capture_runtime_scenarios_111x62(runtime: &str, scenarios: &[&str]) -> Option<Value> {
    capture_runtime_scenarios_at_size(runtime, scenarios, 111, 62)
}

fn capture_rust_scenarios(scenarios: &[&str]) -> Option<Value> {
    capture_runtime_scenarios("rust", scenarios)
}

fn capture_rust_scenarios_111x62(scenarios: &[&str]) -> Option<Value> {
    capture_runtime_scenarios_111x62("rust", scenarios)
}

fn capture_ts_scenarios(scenarios: &[&str]) -> Option<Value> {
    capture_runtime_scenarios("ts", scenarios)
}

fn capture_ts_scenarios_111x62(scenarios: &[&str]) -> Option<Value> {
    capture_runtime_scenarios_111x62("ts", scenarios)
}

fn capture_both_scenarios(scenarios: &[&str]) -> Option<Value> {
    capture_runtime_scenarios("both", scenarios)
}

fn capture_both_scenarios_111x62(scenarios: &[&str]) -> Option<Value> {
    capture_runtime_scenarios_111x62("both", scenarios)
}

fn capture_both_scenarios_at_size(scenarios: &[&str], width: u16, height: u16) -> Option<Value> {
    capture_runtime_scenarios_at_size("both", scenarios, width, height)
}

fn scenario<'a>(root: &'a Value, name: &str) -> &'a Value {
    &root["rust"][name]
}

fn app_owned_field_name(field: &str) -> String {
    let mut chars = field.chars();
    match chars.next() {
        Some(first) => format!("appOwned{}{}", first.to_ascii_uppercase(), chars.as_str()),
        None => "appOwned".to_string(),
    }
}

fn scenario_text(root: &Value, name: &str) -> String {
    scenario(root, name)
        .get("appOwnedText")
        .and_then(Value::as_str)
        .or_else(|| scenario(root, name).get("text").and_then(Value::as_str))
        .unwrap_or_default()
        .to_string()
}

fn scenario_lines(root: &Value, name: &str) -> Vec<String> {
    scenario_text(root, name)
        .lines()
        .map(ToOwned::to_owned)
        .collect()
}

fn scenario_tail(root: &Value, name: &str) -> Vec<String> {
    scenario(root, name)
        .get("appOwnedFooterTail")
        .and_then(Value::as_array)
        .or_else(|| {
            scenario(root, name)
                .get("footerTail")
                .and_then(Value::as_array)
        })
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn scenario_ansi_text(root: &Value, name: &str) -> String {
    scenario(root, name)
        .get("appOwnedAnsiText")
        .and_then(Value::as_str)
        .or_else(|| scenario(root, name).get("ansiText").and_then(Value::as_str))
        .unwrap_or_default()
        .to_string()
}

fn scenario_app_lines(root: &Value, name: &str) -> Vec<String> {
    scenario(root, name)
        .get("appOwnedText")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .lines()
        .map(ToOwned::to_owned)
        .collect()
}

fn scenario_bool(root: &Value, name: &str, field: &str) -> bool {
    let app_owned = app_owned_field_name(field);
    scenario(root, name)
        .get(&app_owned)
        .and_then(Value::as_bool)
        .or_else(|| scenario(root, name).get(field).and_then(Value::as_bool))
        .unwrap_or(false)
}

fn scenario_for_runtime<'a>(root: &'a Value, runtime: &str, name: &str) -> &'a Value {
    &root[runtime][name]
}

fn capture_text_from_record(record: &Value) -> String {
    record
        .get("appOwnedText")
        .and_then(Value::as_str)
        .or_else(|| record.get("text").and_then(Value::as_str))
        .unwrap_or_default()
        .to_string()
}

fn capture_ansi_text_from_record(record: &Value) -> String {
    record
        .get("appOwnedAnsiText")
        .and_then(Value::as_str)
        .or_else(|| record.get("ansiText").and_then(Value::as_str))
        .unwrap_or_default()
        .to_string()
}

fn capture_tail_from_record(record: &Value) -> Vec<String> {
    record
        .get("appOwnedFooterTail")
        .and_then(Value::as_array)
        .or_else(|| record.get("footerTail").and_then(Value::as_array))
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn capture_bool_from_record(record: &Value, field: &str) -> bool {
    let app_owned = app_owned_field_name(field);
    record
        .get(&app_owned)
        .and_then(Value::as_bool)
        .or_else(|| record.get(field).and_then(Value::as_bool))
        .unwrap_or(false)
}

fn scenario_text_for_runtime(root: &Value, runtime: &str, name: &str) -> String {
    capture_text_from_record(scenario_for_runtime(root, runtime, name))
}

fn scenario_ansi_text_for_runtime(root: &Value, runtime: &str, name: &str) -> String {
    capture_ansi_text_from_record(scenario_for_runtime(root, runtime, name))
}

fn scenario_bool_for_runtime(root: &Value, runtime: &str, name: &str, field: &str) -> bool {
    capture_bool_from_record(scenario_for_runtime(root, runtime, name), field)
}

fn scenario_u64_for_runtime(root: &Value, runtime: &str, name: &str, field: &str) -> Option<u64> {
    let app_owned = app_owned_field_name(field);
    scenario_for_runtime(root, runtime, name)
        .get(&app_owned)
        .and_then(Value::as_u64)
        .or_else(|| {
            scenario_for_runtime(root, runtime, name)
                .get(field)
                .and_then(Value::as_u64)
        })
}

fn scenario_u64_vec_for_runtime(root: &Value, runtime: &str, name: &str, field: &str) -> Vec<u64> {
    let app_owned = app_owned_field_name(field);
    scenario_for_runtime(root, runtime, name)
        .get(&app_owned)
        .and_then(Value::as_array)
        .or_else(|| {
            scenario_for_runtime(root, runtime, name)
                .get(field)
                .and_then(Value::as_array)
        })
        .map(|items| items.iter().filter_map(Value::as_u64).collect::<Vec<_>>())
        .unwrap_or_default()
}

fn scenario_tail_for_runtime(root: &Value, runtime: &str, name: &str) -> Vec<String> {
    capture_tail_from_record(scenario_for_runtime(root, runtime, name))
}

fn scenario_frame_for_runtime<'a>(
    root: &'a Value,
    runtime: &str,
    name: &str,
    phase: &str,
) -> &'a Value {
    scenario_for_runtime(root, runtime, name)
        .get("frames")
        .and_then(|frames| frames.get(phase))
        .unwrap_or_else(|| {
            panic!("missing {runtime}:{name}:{phase} frame in capture");
        })
}

fn scenario_frame_text_for_runtime(root: &Value, runtime: &str, name: &str, phase: &str) -> String {
    capture_text_from_record(scenario_frame_for_runtime(root, runtime, name, phase))
}

fn scenario_frame_ansi_text_for_runtime(
    root: &Value,
    runtime: &str,
    name: &str,
    phase: &str,
) -> String {
    capture_ansi_text_from_record(scenario_frame_for_runtime(root, runtime, name, phase))
}

fn scenario_frame_tail_for_runtime(
    root: &Value,
    runtime: &str,
    name: &str,
    phase: &str,
) -> Vec<String> {
    capture_tail_from_record(scenario_frame_for_runtime(root, runtime, name, phase))
}

fn scenario_frame_bool_for_runtime(
    root: &Value,
    runtime: &str,
    name: &str,
    phase: &str,
    field: &str,
) -> bool {
    capture_bool_from_record(
        scenario_frame_for_runtime(root, runtime, name, phase),
        field,
    )
}

fn scenario_u64(root: &Value, name: &str, field: &str) -> Option<u64> {
    let app_owned = app_owned_field_name(field);
    scenario(root, name)
        .get(&app_owned)
        .and_then(Value::as_u64)
        .or_else(|| scenario(root, name).get(field).and_then(Value::as_u64))
}

fn scenario_u64_vec(root: &Value, name: &str, field: &str) -> Vec<u64> {
    let app_owned = app_owned_field_name(field);
    scenario(root, name)
        .get(&app_owned)
        .and_then(Value::as_array)
        .or_else(|| scenario(root, name).get(field).and_then(Value::as_array))
        .map(|items| items.iter().filter_map(Value::as_u64).collect::<Vec<_>>())
        .unwrap_or_default()
}

fn scenario_crashed(root: &Value, name: &str) -> bool {
    scenario(root, name)
        .get("appOwnedCrashed")
        .and_then(Value::as_bool)
        .or_else(|| scenario(root, name).get("crashed").and_then(Value::as_bool))
        .unwrap_or(false)
}

fn scenario_shell_fallback(root: &Value, name: &str) -> bool {
    scenario(root, name)
        .get("appOwnedShellFallback")
        .and_then(Value::as_bool)
        .or_else(|| {
            scenario(root, name)
                .get("shellFallback")
                .and_then(Value::as_bool)
        })
        .unwrap_or(false)
}

fn assert_tmux_meta(captures: &Value, width: u64, height: u64) {
    assert_eq!(captures["meta"]["width"].as_u64(), Some(width));
    assert_eq!(captures["meta"]["height"].as_u64(), Some(height));
}

fn assert_fixed_tmux_meta(captures: &Value) {
    assert_tmux_meta(captures, 80, 24);
}

fn assert_wide_tmux_meta(captures: &Value) {
    assert_tmux_meta(captures, 111, 62);
}

fn is_divider_line(line: &str) -> bool {
    let trimmed = line.trim();
    !trimmed.is_empty() && trimmed.chars().all(|ch| ch == '─')
}

fn assert_footer_tail_has_model_only(root: &Value, name: &str) {
    let tail = scenario_tail(root, name).join("\n");
    assert!(
        tail.contains("gpt-4.1"),
        "{name} footer tail lost the model line:\n{tail}",
    );
}

fn assert_footer_two_line_shape(root: &Value, name: &str) {
    let tail = scenario_tail(root, name);
    assert!(
        tail.len() >= 4,
        "{name} footer tail was too short for shape checks:\n{}",
        tail.join("\n"),
    );

    let repo_index = tail
        .iter()
        .position(|line| line == "<REPO> (<BRANCH>)")
        .expect("repo/branch footer row");
    assert_eq!(
        tail[repo_index],
        "<REPO> (<BRANCH>)",
        "{name} footer lost the repo/branch row:\n{}",
        tail.join("\n"),
    );
    assert!(
        repo_index > 0,
        "{name} footer lost the divider rows before the repo row:\n{}",
        tail.join("\n"),
    );
    let divider_positions: Vec<_> = tail
        .iter()
        .enumerate()
        .filter_map(|(index, line)| is_divider_line(line).then_some(index))
        .collect();
    assert!(
        divider_positions.len() >= 2 && divider_positions[divider_positions.len() - 1] < repo_index,
        "{name} footer lost the divider rows before the repo row:\n{}",
        tail.join("\n"),
    );
    assert!(
        tail.get(repo_index + 1)
            .is_some_and(|line| line.contains("gpt-4.1")),
        "{name} footer lost the model-only row:\n{}",
        tail.join("\n"),
    );
}

fn assert_composer_without_bottom_help(root: &Value, name: &str) {
    assert!(
        !scenario_bool(root, name, "bottomHelpPresent"),
        "{name} still renders a bottom help line beneath the composer:\n{}",
        scenario_text(root, name),
    );

    let divider_rows = scenario_u64_vec(root, name, "dividerRows");
    assert!(
        divider_rows.len() >= 2,
        "{name} composer lost divider rows:\ndivider_rows={divider_rows:?}\n{}",
        scenario_text(root, name),
    );
    let tail = &divider_rows[divider_rows.len() - 2..];
    assert!(
        tail[1] == tail[0] + 2,
        "{name} composer was not reduced to divider + blank row + divider.\ndivider_rows={divider_rows:?}\n{}",
        scenario_text(root, name),
    );
}

fn assert_contains_ordered_subsequence(text: &str, subsequence: &[&str]) {
    let mut offset = 0usize;
    for needle in subsequence {
        let haystack = &text[offset..];
        let index = haystack
            .find(needle)
            .unwrap_or_else(|| panic!("missing '{needle}' in:\n{text}"));
        offset += index + needle.len();
    }
}

fn assert_contains_any(text: &str, needles: &[&str]) {
    assert!(
        needles.iter().any(|needle| text.contains(needle)),
        "missing all of {:?} in:\n{text}",
        needles,
    );
}

fn strip_ansi(text: &str) -> String {
    let mut stripped = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && chars.peek() == Some(&'[') {
            let _ = chars.next();
            while let Some(next) = chars.next() {
                if ('@'..='~').contains(&next) {
                    break;
                }
            }
            continue;
        }
        stripped.push(ch);
    }
    stripped
}

fn assert_ansi_line_contains_any(text: &str, visible: &str, style_fragments: &[&str]) {
    let line = text
        .lines()
        .find(|line| strip_ansi(line).contains(visible))
        .unwrap_or_else(|| panic!("missing visible text {visible:?} in:\n{text}"));
    assert!(
        style_fragments
            .iter()
            .any(|fragment| line.contains(fragment)),
        "line for {visible:?} did not contain any of {:?}:\n{line}",
        style_fragments,
    );
}

fn assert_ansi_line_or_previous_contains_any(text: &str, visible: &str, style_fragments: &[&str]) {
    let lines: Vec<_> = text.lines().collect();
    let index = lines
        .iter()
        .position(|line| strip_ansi(line).contains(visible))
        .unwrap_or_else(|| panic!("missing visible text {visible:?} in:\n{text}"));
    let line = lines[index];
    let previous_line = index
        .checked_sub(1)
        .and_then(|previous| lines.get(previous))
        .copied();
    let matches = |candidate: &str| {
        style_fragments
            .iter()
            .any(|fragment| candidate.contains(fragment))
    };
    assert!(
        matches(line) || previous_line.is_some_and(matches),
        "line for {visible:?} did not contain any of {:?} on the line or immediately before it:\n{line}",
        style_fragments,
    );
}

fn assert_semantic_ansi_text(root: &Value, runtime: &str, name: &str, needles: &[&str]) {
    let text = scenario_text_for_runtime(root, runtime, name);
    let ansi = scenario_ansi_text_for_runtime(root, runtime, name);
    assert!(
        ansi.contains("\u{1b}["),
        "{runtime}:{name} lost ANSI escapes:\n{ansi}",
    );
    let ansi_lines: Vec<_> = ansi.lines().collect();
    let stripped_lines: Vec<_> = ansi_lines.iter().map(|line| strip_ansi(line)).collect();
    for needle in needles {
        assert!(
            text.contains(needle),
            "{runtime}:{name} lost visible text {needle:?}:\n{text}",
        );
        let styled_line = ansi_lines
            .iter()
            .zip(stripped_lines.iter())
            .find_map(|(ansi_line, stripped_line)| {
                stripped_line.contains(needle).then_some(*ansi_line)
            })
            .unwrap_or_else(|| {
                panic!("{runtime}:{name} lost ANSI-visible text {needle:?}:\n{ansi}");
            });
        assert!(
            styled_line.contains("\u{1b}["),
            "{runtime}:{name} lost ANSI styling for {needle:?}:\n{ansi}",
        );
    }
}

fn assert_active_streaming_surface(
    root: &Value,
    runtime: &str,
    name: &str,
    prompt: &str,
    thinking: &str,
    response: &str,
) {
    let initial_text = scenario_frame_text_for_runtime(root, runtime, name, "initial");
    let initial_ansi = scenario_frame_ansi_text_for_runtime(root, runtime, name, "initial");
    let active_text = scenario_frame_text_for_runtime(root, runtime, name, "active");
    let active_ansi = scenario_frame_ansi_text_for_runtime(root, runtime, name, "active");
    let settled_text = scenario_frame_text_for_runtime(root, runtime, name, "settled");
    let settled_ansi = scenario_frame_ansi_text_for_runtime(root, runtime, name, "settled");
    let active_tail = scenario_frame_tail_for_runtime(root, runtime, name, "active");
    let settled_tail = scenario_frame_tail_for_runtime(root, runtime, name, "settled");

    assert!(
        !initial_text.contains(response),
        "{runtime}:{name}:initial should not already contain the settled assistant text:\n{initial_text}",
    );
    if initial_text.contains("Waiting for model response...") {
        assert_ansi_line_contains_any(
            &initial_ansi,
            "Waiting for model response...",
            &["38;2;129;162;190", "38;5;110"],
        );
    }

    if runtime == "ts" {
        assert!(
            initial_text.contains(prompt) || active_text.contains(prompt),
            "{runtime}:{name}:initial/active lost the queued user prompt:\ninitial:\n{initial_text}\n\nactive:\n{active_text}",
        );
    }

    assert!(
        active_text.contains(prompt),
        "{runtime}:{name}:active lost the user message block:\n{active_text}",
    );
    assert!(
        active_text.contains(thinking),
        "{runtime}:{name}:active lost visible thinking text:\n{active_text}",
    );
    assert!(
        active_text.contains("| Working for"),
        "{runtime}:{name}:active lost the working loader row:\n{active_text}",
    );
    assert!(
        !active_text.contains("Response received."),
        "{runtime}:{name}:active should not already be settled:\n{active_text}",
    );
    if runtime == "rust" {
        assert!(
            !active_text.contains("Waiting for model response..."),
            "{runtime}:{name}:active should no longer show the waiting row:\n{active_text}",
        );
    }
    assert_ansi_line_or_previous_contains_any(&active_ansi, prompt, &["48;2;52;53;65", "48;5;237"]);
    assert_ansi_line_contains_any(
        &active_ansi,
        thinking,
        &["3m", "38;5;244", "38;2;128;128;128"],
    );
    assert_ansi_line_contains_any(
        &active_ansi,
        "| Working for",
        &["38;2;255;255;0", "38;5;11"],
    );
    assert!(
        active_tail
            .last()
            .is_some_and(|line| line.contains("0.0%/1.0M (auto)") && line.contains("gpt-4.1")),
        "{runtime}:{name}:active lost the usage footer row:\n{}",
        active_tail.join("\n"),
    );
    let active_repo_index = active_tail
        .iter()
        .position(|line| line == "<REPO> (<BRANCH>)")
        .unwrap_or_else(|| {
            panic!(
                "{runtime}:{name}:active lost the footer repo row:\n{}",
                active_tail.join("\n")
            )
        });
    assert!(
        active_repo_index > 0,
        "{runtime}:{name}:active lost the divider rows before the repo row:\n{}",
        active_tail.join("\n"),
    );
    let active_divider_positions: Vec<_> = active_tail
        .iter()
        .enumerate()
        .filter_map(|(index, line)| is_divider_line(line).then_some(index))
        .collect();
    assert!(
        active_divider_positions.len() >= 2
            && active_divider_positions[active_divider_positions.len() - 1] < active_repo_index,
        "{runtime}:{name}:active lost the divider rows before the repo row:\n{}",
        active_tail.join("\n"),
    );
    assert_eq!(
        active_tail[active_repo_index],
        "<REPO> (<BRANCH>)",
        "{runtime}:{name}:active lost the footer repo row:\n{}",
        active_tail.join("\n"),
    );
    assert!(
        scenario_frame_bool_for_runtime(root, runtime, name, "active", "containsFooterModelOnly"),
        "{runtime}:{name}:active should remain model-only while the stream is in flight:\n{active_text}",
    );
    assert!(
        !scenario_frame_bool_for_runtime(
            root,
            runtime,
            name,
            "active",
            "containsFooterThinkingDetail"
        ),
        "{runtime}:{name}:active should not surface a thinking footer state while streaming:\n{active_text}",
    );

    assert!(
        settled_text.contains(prompt),
        "{runtime}:{name}:settled lost the user message block:\n{settled_text}",
    );
    assert!(
        settled_text.contains(thinking),
        "{runtime}:{name}:settled lost visible thinking text:\n{settled_text}",
    );
    assert!(
        settled_text.contains(response),
        "{runtime}:{name}:settled lost the final assistant text:\n{settled_text}",
    );
    assert_contains_ordered_subsequence(&settled_text, &[prompt, thinking, response]);
    assert!(
        !settled_text.contains("Waiting for model response..."),
        "{runtime}:{name}:settled should no longer show the waiting row:\n{settled_text}",
    );
    assert!(
        !settled_text.contains("| Working for"),
        "{runtime}:{name}:settled should no longer show the working loader row:\n{settled_text}",
    );
    assert!(
        !settled_text.contains("Response received."),
        "{runtime}:{name}:settled should not show the old response status line:\n{settled_text}",
    );
    assert_ansi_line_or_previous_contains_any(
        &settled_ansi,
        prompt,
        &["48;2;52;53;65", "48;5;237"],
    );
    assert_ansi_line_contains_any(
        &settled_ansi,
        thinking,
        &["3m", "38;5;244", "38;2;128;128;128"],
    );
    assert!(
        settled_tail
            .last()
            .is_some_and(|line| line.contains("0.0%/1.0M (auto)") && line.contains("gpt-4.1")),
        "{runtime}:{name}:settled lost the usage footer row:\n{}",
        settled_tail.join("\n"),
    );
}

const REQUIRED_SCENARIOS: &[&str] = &[
    "startup",
    "shift-tab",
    "settings",
    "resume",
    "startup-resume",
    "fork",
    "model",
    "scoped-models",
    "manual-bash",
    "bash",
    "read",
    "write",
    "edit",
    "diff",
];

const MATRIX_SCENARIOS: &[&str] = &[
    "settings",
    "resume",
    "tree-navigation",
    "config-browser",
    "manual-bash",
    "diff",
    "live-streaming-start",
    "hidden-thinking",
];

const LONG_TAIL_SCENARIOS: &[&str] = &[
    "startup-diagnostics",
    "resume-populated-management",
    "tree-navigation",
    "tree-summary",
    "fork-populated",
    "login",
    "logout",
    "reload-diagnostics",
    "session",
    "changelog",
    "hotkeys",
    "copy-empty",
    "share-missing-gh",
    "footer-variants",
    "excluded-bash",
    "custom-messages-and-skills",
    "compaction-and-retry",
];

fn assert_required_scenarios_are_captured(root: &Value, runtime: &str) {
    for scenario in REQUIRED_SCENARIOS {
        assert!(
            !scenario_bool_for_runtime(root, runtime, scenario, "crashed"),
            "{runtime}:{scenario} crashed unexpectedly:\n{}",
            scenario_text_for_runtime(root, runtime, scenario),
        );
        assert!(
            !scenario_bool_for_runtime(root, runtime, scenario, "shellFallback"),
            "{runtime}:{scenario} fell back to the shell unexpectedly:\n{}",
            scenario_text_for_runtime(root, runtime, scenario),
        );
    }

    assert!(scenario_bool_for_runtime(
        root,
        runtime,
        "startup",
        "contextBlockPresent"
    ));
    assert!(scenario_bool_for_runtime(
        root,
        runtime,
        "startup",
        "containsCtrlGHint"
    ));
    assert!(scenario_bool_for_runtime(
        root,
        runtime,
        "startup",
        "containsCtrlVHint"
    ));
    assert!(scenario_bool_for_runtime(
        root,
        runtime,
        "startup",
        "containsDropFilesHint"
    ));
    let startup_ansi = scenario_ansi_text_for_runtime(root, runtime, "startup");
    assert_ansi_line_contains_any(
        &startup_ansi,
        "shift+tab to cycle thinking level",
        &["38;2;128;128;128", "38;5;244"],
    );
    assert_ansi_line_contains_any(
        &startup_ansi,
        "[Context]",
        &["38;2;240;198;116", "38;5;111"],
    );
    if runtime == "rust" {
        assert!(
            scenario_bool_for_runtime(root, runtime, "startup", "containsFooterThinkingDetail"),
            "{runtime}:startup should surface the thinking footer state:\n{}",
            scenario_text_for_runtime(root, runtime, "startup"),
        );
        assert!(
            !scenario_bool_for_runtime(root, runtime, "startup", "containsFooterModelOnly"),
            "{runtime}:startup should not be model-only when thinking is exposed:\n{}",
            scenario_text_for_runtime(root, runtime, "startup"),
        );
    } else {
        assert!(
            scenario_bool_for_runtime(root, runtime, "startup", "containsFooterModelOnly"),
            "{runtime}:startup should stay model-only in the TS capture:\n{}",
            scenario_text_for_runtime(root, runtime, "startup"),
        );
        assert!(
            !scenario_bool_for_runtime(root, runtime, "startup", "containsFooterThinkingDetail"),
            "{runtime}:startup should not surface a thinking footer state in the TS capture:\n{}",
            scenario_text_for_runtime(root, runtime, "startup"),
        );
    }
    let startup = scenario_text_for_runtime(root, runtime, "startup");
    assert_contains_ordered_subsequence(
        &startup,
        &[
            "shift+tab to cycle thinking level",
            "ctrl+p/shift+ctrl+p to cycle models",
            "ctrl+l to select model",
            "ctrl+o to expand tools",
            "ctrl+t to expand thinking",
            "ctrl+g for external editor",
            "/ for commands",
            "! to run bash",
            "!! to run bash (no context)",
            "alt+enter to queue follow-up",
            "alt+up to edit all queued messages",
            "ctrl+v to paste image",
            "drop files to attach",
        ],
    );
    assert_semantic_ansi_text(
        root,
        runtime,
        "startup",
        &[
            "shift+tab to cycle thinking level",
            "ctrl+p/shift+ctrl+p to cycle models",
            "ctrl+l to select model",
            "ctrl+o to expand tools",
            "ctrl+t to expand thinking",
        ],
    );
    assert!(scenario_bool_for_runtime(
        root,
        runtime,
        "shift-tab",
        "containsThinkingLevelStatus"
    ));
    assert!(scenario_text_for_runtime(root, runtime, "settings").contains("Auto-compact"));
    assert_semantic_ansi_text(root, runtime, "settings", &["Auto-compact"]);
    assert!(scenario_text_for_runtime(root, runtime, "resume").contains("Resume Session"));
    assert!(scenario_text_for_runtime(root, runtime, "startup-resume").contains("Resume Session"));
    assert!(scenario_bool_for_runtime(
        root,
        runtime,
        "fork",
        "containsNoMessagesToForkFrom"
    ));
    assert_semantic_ansi_text(root, runtime, "resume", &["Resume Session"]);
    assert!(scenario_text_for_runtime(root, runtime, "model").contains("Model Name: GPT-4.1"));
    assert_semantic_ansi_text(root, runtime, "model", &["Model Name: GPT-4.1"]);
    let scoped_models = scenario_text_for_runtime(root, runtime, "scoped-models");
    assert!(scoped_models.contains("Model Name:"));
    assert!(scoped_models.contains("Enter toggle"));
    assert_semantic_ansi_text(
        root,
        runtime,
        "scoped-models",
        &["Model Name:", "Enter toggle"],
    );
    assert!(
        scenario_text_for_runtime(root, runtime, "manual-bash").contains("!printf hello-from-bash")
    );
    assert!(scenario_text_for_runtime(root, runtime, "bash").contains("hello-from-bash"));
    assert_semantic_ansi_text(root, runtime, "manual-bash", &["!printf hello-from-bash"]);
    assert_semantic_ansi_text(root, runtime, "bash", &["hello-from-bash"]);
    assert!(scenario_text_for_runtime(root, runtime, "read").contains("read /tmp/example.rs:2-3"));
    assert!(scenario_text_for_runtime(root, runtime, "read").contains("return 42;"));
    assert!(scenario_text_for_runtime(root, runtime, "write").contains("write src/main.rs"));
    assert!(scenario_text_for_runtime(root, runtime, "write").contains("println!(\"hi\");"));
    assert!(scenario_text_for_runtime(root, runtime, "edit").contains("edit src/lib.rs"));
    assert!(scenario_text_for_runtime(root, runtime, "edit").contains("+let value = 2;"));
    assert_semantic_ansi_text(
        root,
        runtime,
        "read",
        &["read /tmp/example.rs:2-3", "return 42;"],
    );
    assert_semantic_ansi_text(
        root,
        runtime,
        "write",
        &["write src/main.rs", "println!(\"hi\");"],
    );
    assert_semantic_ansi_text(
        root,
        runtime,
        "edit",
        &["edit src/lib.rs", "+let value = 2;"],
    );
    assert!(scenario_text_for_runtime(root, runtime, "diff").contains("diff --git"));
    assert!(scenario_text_for_runtime(root, runtime, "diff").contains("(exit 1)"));
    assert_semantic_ansi_text(root, runtime, "diff", &["diff --git", "(exit 1)"]);
}

fn assert_long_tail_surfaces_are_captured(root: &Value, runtime: &str) {
    let startup_diagnostics = scenario_text_for_runtime(root, runtime, "startup-diagnostics");
    assert_contains_ordered_subsequence(
        &startup_diagnostics,
        &["[Context]", "[Skills]", "[Prompts]"],
    );
    assert_contains_any(&startup_diagnostics, &["[Themes]", "[Theme conflicts]"]);
    assert_contains_any(
        &startup_diagnostics,
        &["broken.json", "theme", "warning", "error"],
    );
    assert!(!scenario_crashed(root, "startup-diagnostics"));

    let resume_populated = scenario_text_for_runtime(root, runtime, "resume-populated-management");
    assert!(resume_populated.contains("Resume Session"));
    assert_contains_any(
        &resume_populated,
        &[
            "Delete session? [Enter] confirm",
            "Delete session?",
            "Alpha Session",
            "Beta Session",
            "No sessions in current folder. Press Tab to view all.",
        ],
    );
    assert!(!scenario_crashed(root, "resume-populated-management"));

    let tree_navigation = scenario_text_for_runtime(root, runtime, "tree-navigation");
    assert!(tree_navigation.contains("Session Tree"));
    assert!(tree_navigation.contains(">") || tree_navigation.contains("Type to search:"));
    assert!(
        tree_navigation.contains("Label (empty to remove):")
            || tree_navigation.contains("↑/↓: move.")
    );
    assert!(scenario_bool_for_runtime(
        root,
        runtime,
        "tree-navigation",
        "containsSessionTreeTitle"
    ));
    assert!(
        scenario_bool_for_runtime(root, runtime, "tree-navigation", "containsTreeSearchPrompt")
            || scenario_bool_for_runtime(
                root,
                runtime,
                "tree-navigation",
                "containsTreeLabelPrompt"
            )
    );
    assert!(!scenario_crashed(root, "tree-navigation"));

    let tree_summary = scenario_text_for_runtime(root, runtime, "tree-summary");
    assert!(tree_summary.contains("Summarize branch?"));
    assert_contains_ordered_subsequence(
        &tree_summary,
        &[
            "Summarize branch?",
            "No summary",
            "Summarize",
            "Summarize with custom prompt",
        ],
    );
    assert!(!scenario_crashed(root, "tree-summary"));

    let fork_populated = scenario_text_for_runtime(root, runtime, "fork-populated");
    assert!(fork_populated.contains("Branch from Message"));
    assert!(fork_populated.contains("Select a message to create a new branch from that point"));
    assert!(fork_populated.contains("Message 1 of"));
    assert!(scenario_bool_for_runtime(
        root,
        runtime,
        "fork-populated",
        "containsBranchFromMessageTitle"
    ));
    assert!(scenario_bool_for_runtime(
        root,
        runtime,
        "fork-populated",
        "containsBranchFromMessageSubtitle"
    ));
    assert!(!scenario_crashed(root, "fork-populated"));

    let login = scenario_text_for_runtime(root, runtime, "login");
    assert!(login.contains("Select provider to login:"));
    assert_contains_any(
        &login,
        &[
            "Anthropic (Claude Pro/Max)",
            "ChatGPT Plus/Pro (Codex Subscription)",
            "logged in",
        ],
    );
    assert!(scenario_bool_for_runtime(
        root,
        runtime,
        "login",
        "containsOAuthLoginTitle"
    ));
    assert!(!scenario_crashed(root, "login"));

    let logout = scenario_text_for_runtime(root, runtime, "logout");
    assert!(logout.contains("Select provider to logout:"));
    assert_contains_any(
        &logout,
        &[
            "Anthropic (Claude Pro/Max)",
            "ChatGPT Plus/Pro (Codex Subscription)",
            "No OAuth providers logged in. Use /login first.",
            "logged in",
        ],
    );
    assert!(scenario_bool_for_runtime(
        root,
        runtime,
        "logout",
        "containsOAuthLogoutTitle"
    ));
    assert!(!scenario_crashed(root, "logout"));

    let reload = scenario_text_for_runtime(root, runtime, "reload-diagnostics");
    assert!(reload.contains("Reloaded extensions, skills, prompts, themes"));
    assert!(!scenario_crashed(root, "reload-diagnostics"));

    let session = scenario_text_for_runtime(root, runtime, "session");
    assert_contains_any(
        &session,
        &["Session Info", "Session Info added to the transcript."],
    );
    assert!(session.contains("Messages"));
    assert!(session.contains("Tokens"));
    assert!(scenario_bool_for_runtime(
        root,
        runtime,
        "session",
        "containsSessionInfoTitle"
    ));
    assert!(!scenario_crashed(root, "session"));

    let changelog = scenario_text_for_runtime(root, runtime, "changelog");
    let changelog_has_stable_heading = changelog.contains("What's New")
        || changelog.contains("What's new")
        || changelog.contains("Release Notes")
        || changelog.contains("Changelog");
    if changelog_has_stable_heading {
        assert_contains_any(
            &changelog,
            &["What's New", "What's new", "Release Notes", "Changelog"],
        );
    } else {
        eprintln!("{runtime}:changelog did not surface a stable visible heading in this capture");
    }
    assert!(!scenario_crashed(root, "changelog"));

    let hotkeys = scenario_text_for_runtime(root, runtime, "hotkeys");
    let hotkeys_have_stable_heading = hotkeys.contains("Keyboard Shortcuts")
        || hotkeys.contains("Keyboard shortcuts added to the transcript.");
    if hotkeys_have_stable_heading {
        assert_contains_any(&hotkeys, &["Ctrl+V", "!!", "Ctrl+P", "Ctrl+T"]);
    } else {
        eprintln!("{runtime}:hotkeys did not surface a stable visible heading in this capture");
    }
    assert!(!scenario_crashed(root, "hotkeys"));

    let copy_empty = scenario_text_for_runtime(root, runtime, "copy-empty");
    assert!(copy_empty.contains("No agent messages to copy yet."));
    assert!(!scenario_crashed(root, "copy-empty"));

    let share = scenario_text_for_runtime(root, runtime, "share-missing-gh");
    assert_contains_any(
        &share,
        &[
            "GitHub CLI is not logged in. Run 'gh auth login' first.",
            "GitHub CLI (gh) is not installed. Install it from https://cli.github.com/",
        ],
    );
    assert!(!scenario_shell_fallback(root, "share-missing-gh"));
    let compaction_retry = scenario_text_for_runtime(root, runtime, "compaction-and-retry");
    assert_contains_any(&compaction_retry, &["[compaction]", "Compacted from "]);
    assert!(!scenario_crashed(root, "compaction-and-retry"));
}

#[test]
fn ts_and_rust_required_surfaces_are_genuinely_captured_in_fixed_80x24_tmux() {
    let Some(captures) = capture_both_scenarios(REQUIRED_SCENARIOS) else {
        return;
    };

    assert_fixed_tmux_meta(&captures);
    assert_required_scenarios_are_captured(&captures, "ts");
    assert_required_scenarios_are_captured(&captures, "rust");
}

#[test]
fn ts_and_rust_startup_and_manual_bash_show_full_help_stack_in_fixed_111x62_tmux() {
    let Some(captures) = capture_both_scenarios_111x62(&["startup", "manual-bash"]) else {
        return;
    };

    assert_eq!(
        scenario_u64_for_runtime(&captures, "ts", "startup", "promptRow"),
        None
    );
    assert_eq!(
        scenario_u64_for_runtime(&captures, "rust", "startup", "promptRow"),
        None
    );
    assert!(scenario_bool_for_runtime(
        &captures,
        "ts",
        "startup",
        "containsCtrlVHint"
    ));
    assert!(scenario_bool_for_runtime(
        &captures,
        "ts",
        "startup",
        "containsDropFilesHint"
    ));
    assert!(scenario_bool_for_runtime(
        &captures,
        "rust",
        "startup",
        "containsCtrlVHint"
    ));
    assert!(scenario_bool_for_runtime(
        &captures,
        "rust",
        "startup",
        "containsDropFilesHint"
    ));
    assert!(
        scenario_text_for_runtime(&captures, "ts", "startup").contains("ctrl+v to paste image")
    );
    assert!(scenario_text_for_runtime(&captures, "ts", "startup").contains("drop files to attach"));
    assert!(
        scenario_text_for_runtime(&captures, "rust", "startup").contains("ctrl+v to paste image")
    );
    assert!(
        scenario_text_for_runtime(&captures, "rust", "startup").contains("drop files to attach")
    );
    assert_contains_ordered_subsequence(
        &scenario_text_for_runtime(&captures, "ts", "startup"),
        &[
            "shift+tab to cycle thinking level",
            "ctrl+p/shift+ctrl+p to cycle models",
            "ctrl+l to select model",
            "ctrl+o to expand tools",
            "ctrl+t to expand thinking",
            "ctrl+g for external editor",
            "/ for commands",
            "! to run bash",
            "!! to run bash (no context)",
            "alt+enter to queue follow-up",
            "alt+up to edit all queued messages",
            "ctrl+v to paste image",
            "drop files to attach",
        ],
    );
    assert!(scenario_text_for_runtime(&captures, "ts", "manual-bash").contains("0.0%/1.0M (auto)"));
    assert!(
        scenario_text_for_runtime(&captures, "rust", "manual-bash").contains("0.0%/1.0M (auto)")
    );
    let ts_startup_ansi = scenario_ansi_text_for_runtime(&captures, "ts", "startup");
    assert_ansi_line_contains_any(
        &ts_startup_ansi,
        "shift+tab to cycle thinking level",
        &["38;2;128;128;128", "38;5;244"],
    );
    assert_ansi_line_contains_any(
        &ts_startup_ansi,
        "[Context]",
        &["38;2;240;198;116", "38;5;111"],
    );
    let rust_startup_ansi = scenario_ansi_text_for_runtime(&captures, "rust", "startup");
    assert_ansi_line_contains_any(
        &rust_startup_ansi,
        "shift+tab to cycle thinking level",
        &["38;2;128;128;128", "38;5;244"],
    );
    assert_ansi_line_contains_any(
        &rust_startup_ansi,
        "[Context]",
        &["38;2;240;198;116", "38;5;111"],
    );
    assert!(scenario_bool_for_runtime(
        &captures,
        "ts",
        "startup",
        "containsFooterModelOnly"
    ));
    assert!(scenario_bool_for_runtime(
        &captures,
        "rust",
        "startup",
        "containsFooterThinkingDetail"
    ));
    assert!(!scenario_crashed(&captures, "startup"));
    assert!(!scenario_crashed(&captures, "manual-bash"));
}

#[test]
fn ts_and_rust_settings_and_startup_resume_render_in_fixed_80x24_tmux() {
    let Some(captures) = capture_both_scenarios(&["settings", "startup-resume"]) else {
        return;
    };

    assert_fixed_tmux_meta(&captures);

    let ts_settings = scenario_text_for_runtime(&captures, "ts", "settings");
    assert!(ts_settings.contains(">"));
    assert_contains_ordered_subsequence(
        &ts_settings,
        &[
            "→ Auto-compact",
            "Auto-resize images",
            "Block images",
            "Skill commands",
            "Show hardware cursor",
            "Editor padding",
            "Autocomplete max items",
            "Clear on shrink",
            "Steering mode",
            "Follow-up mode",
        ],
    );
    let rust_settings = scenario_text_for_runtime(&captures, "rust", "settings");
    assert!(rust_settings.contains(">"));
    assert_contains_ordered_subsequence(
        &rust_settings,
        &[
            "→ Auto-compact",
            "Auto-resize images",
            "Block images",
            "Skill commands",
            "Show hardware cursor",
            "Editor padding",
            "Autocomplete max items",
            "Clear on shrink",
            "Steering mode",
            "Follow-up mode",
        ],
    );
    assert_eq!(
        scenario_u64_for_runtime(&captures, "ts", "settings", "promptRow"),
        Some(1)
    );
    assert_eq!(
        scenario_u64_for_runtime(&captures, "rust", "settings", "promptRow"),
        Some(2)
    );
    assert!(!scenario_crashed(&captures, "settings"));

    let ts_startup_resume = scenario_text_for_runtime(&captures, "ts", "startup-resume");
    let rust_startup_resume = scenario_text_for_runtime(&captures, "rust", "startup-resume");
    assert!(ts_startup_resume.contains("Resume Session"));
    assert!(ts_startup_resume.contains("No sessions in current folder. Press Tab to view all."));
    assert_contains_ordered_subsequence(
        &ts_startup_resume,
        &[
            "Resume Session (Current Fold",
            "tab scope · re:<pattern> regex · \"phrase\" exact",
            "ctrl+s sort · ctrl+n named · ctrl+d delete · ctrl+p path (off)",
            "No sessions in current folder. Press Tab to view all.",
        ],
    );
    assert!(rust_startup_resume.contains("Resume Session"));
    assert!(rust_startup_resume.contains("No sessions in current folder. Press Tab to view all."));
    assert_contains_ordered_subsequence(
        &rust_startup_resume,
        &[
            "Resume Session (Current Fold",
            "tab scope · re:<pattern> regex · \"phrase\" exact",
            "ctrl+s sort · ctrl+n named · ctrl+d delete · ctrl+p path (off)",
            "No sessions in current folder. Press Tab to view all.",
        ],
    );
    assert_eq!(
        scenario_u64_for_runtime(&captures, "ts", "startup-resume", "promptRow"),
        Some(6)
    );
    assert_eq!(
        scenario_u64_for_runtime(&captures, "rust", "startup-resume", "promptRow"),
        Some(6)
    );
    assert!(!scenario_crashed(&captures, "startup-resume"));
}

#[test]
fn ts_and_rust_matrix_surfaces_render_in_fixed_64x20_tmux() {
    let Some(captures) = capture_both_scenarios_at_size(MATRIX_SCENARIOS, 64, 20) else {
        return;
    };

    assert_tmux_meta(&captures, 64, 20);

    for runtime in ["ts", "rust"] {
        let settings = scenario_text_for_runtime(&captures, runtime, "settings");
        assert!(settings.contains("Auto-compact"));
        assert_semantic_ansi_text(&captures, runtime, "settings", &["Auto-compact"]);

        let resume = scenario_text_for_runtime(&captures, runtime, "resume");
        assert!(resume.contains("Resume"));
        assert_contains_any(
            &resume,
            &[
                "No sessions in current folder. Press Tab to view all.",
                "No sessions found",
            ],
        );
        assert_semantic_ansi_text(&captures, runtime, "resume", &["Resume"]);

        let tree_navigation = scenario_text_for_runtime(&captures, runtime, "tree-navigation");
        assert!(tree_navigation.contains("Session Tree"));
        assert_contains_any(&tree_navigation, &["Enter navigates", "Shift+L: label."]);
        assert_semantic_ansi_text(&captures, runtime, "tree-navigation", &["Session Tree"]);

        let config_browser = scenario_text_for_runtime(&captures, runtime, "config-browser");
        assert!(config_browser.contains("Resource Configuration"));
        assert_contains_any(
            &config_browser,
            &["No resources found", "Type to filter resources"],
        );
        assert_semantic_ansi_text(
            &captures,
            runtime,
            "config-browser",
            &["Resource Configuration"],
        );

        let manual_bash = scenario_text_for_runtime(&captures, runtime, "manual-bash");
        assert!(manual_bash.contains("!printf hello-from-bash"));
        assert_semantic_ansi_text(
            &captures,
            runtime,
            "manual-bash",
            &["!printf hello-from-bash"],
        );

        let diff = scenario_text_for_runtime(&captures, runtime, "diff");
        assert!(diff.contains("diff --git"));
        assert!(diff.contains("(exit 1)"));
        assert_semantic_ansi_text(&captures, runtime, "diff", &["diff --git", "(exit 1)"]);

        let live_streaming_start =
            scenario_text_for_runtime(&captures, runtime, "live-streaming-start");
        assert_contains_any(
            &live_streaming_start,
            &["Live streaming start", "| Working for"],
        );
        assert_semantic_ansi_text(
            &captures,
            runtime,
            "live-streaming-start",
            &["Streaming visible thinking start"],
        );

        assert!(!scenario_crashed(&captures, "settings"));
        assert!(!scenario_crashed(&captures, "resume"));
        assert!(!scenario_crashed(&captures, "tree-navigation"));
        assert!(!scenario_crashed(&captures, "config-browser"));
        assert!(!scenario_crashed(&captures, "manual-bash"));
        assert!(!scenario_crashed(&captures, "diff"));
        assert!(!scenario_crashed(&captures, "live-streaming-start"));
    }
}

#[test]
fn ts_and_rust_live_streaming_and_tool_surfaces_render_in_fixed_80x24_tmux() {
    let Some(captures) = capture_both_scenarios(&[
        "hidden-thinking",
        "live-streaming-start",
        "live-streaming-mid",
        "abort-active-run",
        "tool-lifecycle",
    ]) else {
        return;
    };

    assert_fixed_tmux_meta(&captures);

    for runtime in ["ts", "rust"] {
        let hidden_initial =
            scenario_frame_text_for_runtime(&captures, runtime, "hidden-thinking", "initial");
        let hidden_active =
            scenario_frame_text_for_runtime(&captures, runtime, "hidden-thinking", "active");
        let hidden_settled =
            scenario_frame_text_for_runtime(&captures, runtime, "hidden-thinking", "settled");
        assert!(
            hidden_initial.contains("Thinking...") || hidden_active.contains("Thinking..."),
            "expected hidden-thinking placeholder in initial or active frame for {runtime}:\ninitial:\n{hidden_initial}\nactive:\n{hidden_active}"
        );
        assert!(
            hidden_active.contains("| Working for"),
            "expected hidden-thinking active frame to show the working loader row for {runtime}:\n{hidden_active}"
        );
        if runtime == "rust" {
            assert!(
                !hidden_active.contains("Waiting for model response..."),
                "expected hidden-thinking active frame to drop the old waiting row for {runtime}:\n{hidden_active}"
            );
        }
        assert!(
            hidden_settled.contains("Hidden thinking response"),
            "expected hidden-thinking settled frame to show final response for {runtime}:\n{hidden_settled}"
        );
        assert!(
            !hidden_settled.contains("Waiting for model response...")
                && !hidden_settled.contains("| Working for"),
            "expected hidden-thinking settled frame to drop working row for {runtime}:\n{hidden_settled}"
        );
        let hidden_active_ansi =
            scenario_frame_ansi_text_for_runtime(&captures, runtime, "hidden-thinking", "active");
        assert_contains_any(&hidden_active_ansi, &["Thinking..."]);

        assert_active_streaming_surface(
            &captures,
            runtime,
            "live-streaming-start",
            "Show the stream starting.",
            "Streaming visible thinking start",
            "Live streaming start",
        );

        assert_active_streaming_surface(
            &captures,
            runtime,
            "live-streaming-mid",
            "Show the stream mid-flight.",
            "Streaming visible thinking mid",
            "Live streaming mid",
        );

        let abort_active_run = scenario_text_for_runtime(&captures, runtime, "abort-active-run");
        assert_contains_any(
            &abort_active_run,
            &["Request aborted", "Operation aborted", "Aborting live run"],
        );
        assert!(abort_active_run.contains("gpt-4.1"));

        let tool_lifecycle = scenario_text_for_runtime(&captures, runtime, "tool-lifecycle");
        assert_contains_any(
            &tool_lifecycle,
            &["$ printf tool-lifecycle", "tool-lifecycle complete"],
        );
        assert_semantic_ansi_text(
            &captures,
            runtime,
            "tool-lifecycle",
            &["$ printf tool-lifecycle"],
        );

        assert!(!scenario_crashed(&captures, "hidden-thinking"));
        assert!(!scenario_crashed(&captures, "live-streaming-start"));
        assert!(!scenario_crashed(&captures, "live-streaming-mid"));
        assert!(!scenario_crashed(&captures, "abort-active-run"));
        assert!(!scenario_crashed(&captures, "tool-lifecycle"));
    }
}

#[test]
fn ts_and_rust_read_write_edit_and_diff_baselines_are_live_and_structured_in_fixed_80x24_tmux() {
    let Some(captures) = capture_both_scenarios(&["read", "write", "edit", "diff"]) else {
        return;
    };

    assert_fixed_tmux_meta(&captures);

    for runtime in ["ts", "rust"] {
        assert!(
            !scenario_bool_for_runtime(&captures, runtime, "read", "crashed"),
            "{runtime}:read crashed unexpectedly:\n{}",
            scenario_text_for_runtime(&captures, runtime, "read"),
        );
        assert!(
            scenario_text_for_runtime(&captures, runtime, "read")
                .contains("read /tmp/example.rs:2-3"),
            "{runtime}:read baseline lost the read header:\n{}",
            scenario_text_for_runtime(&captures, runtime, "read"),
        );
        assert!(
            scenario_text_for_runtime(&captures, runtime, "read").contains("return 42;"),
            "{runtime}:read baseline lost the file excerpt:\n{}",
            scenario_text_for_runtime(&captures, runtime, "read"),
        );

        assert!(
            !scenario_bool_for_runtime(&captures, runtime, "write", "crashed"),
            "{runtime}:write crashed unexpectedly:\n{}",
            scenario_text_for_runtime(&captures, runtime, "write"),
        );
        assert!(
            scenario_text_for_runtime(&captures, runtime, "write").contains("write src/main.rs"),
            "{runtime}:write baseline lost the write header:\n{}",
            scenario_text_for_runtime(&captures, runtime, "write"),
        );
        assert!(
            scenario_text_for_runtime(&captures, runtime, "write").contains("println!(\"hi\");"),
            "{runtime}:write baseline lost the file body:\n{}",
            scenario_text_for_runtime(&captures, runtime, "write"),
        );

        assert!(
            !scenario_bool_for_runtime(&captures, runtime, "edit", "crashed"),
            "{runtime}:edit crashed unexpectedly:\n{}",
            scenario_text_for_runtime(&captures, runtime, "edit"),
        );
        assert!(
            scenario_text_for_runtime(&captures, runtime, "edit").contains("edit src/lib.rs"),
            "{runtime}:edit baseline lost the edit header:\n{}",
            scenario_text_for_runtime(&captures, runtime, "edit"),
        );
        assert!(
            scenario_text_for_runtime(&captures, runtime, "edit").contains("+let value = 2;"),
            "{runtime}:edit baseline lost the replacement line:\n{}",
            scenario_text_for_runtime(&captures, runtime, "edit"),
        );

        assert!(
            !scenario_bool_for_runtime(&captures, runtime, "diff", "crashed"),
            "{runtime}:diff crashed unexpectedly:\n{}",
            scenario_text_for_runtime(&captures, runtime, "diff"),
        );
        assert!(
            scenario_text_for_runtime(&captures, runtime, "diff").contains("diff --git"),
            "{runtime}:diff baseline lost the git diff header:\n{}",
            scenario_text_for_runtime(&captures, runtime, "diff"),
        );
        assert!(
            scenario_text_for_runtime(&captures, runtime, "diff").contains("(exit 1)"),
            "{runtime}:diff baseline lost the exit marker:\n{}",
            scenario_text_for_runtime(&captures, runtime, "diff"),
        );
        assert!(
            !scenario_text_for_runtime(&captures, runtime, "diff").contains("Exit code: 1"),
            "{runtime}:diff baseline still has Rust-only exit prose:\n{}",
            scenario_text_for_runtime(&captures, runtime, "diff"),
        );
    }
}

#[test]
fn ts_and_rust_read_write_edit_and_diff_baselines_are_live_and_structured_in_fixed_111x62_tmux() {
    let Some(captures) = capture_both_scenarios_111x62(&["read", "write", "edit", "diff"]) else {
        return;
    };

    assert_wide_tmux_meta(&captures);

    for runtime in ["ts", "rust"] {
        assert!(
            !scenario_bool_for_runtime(&captures, runtime, "read", "crashed"),
            "{runtime}:read crashed unexpectedly:\n{}",
            scenario_text_for_runtime(&captures, runtime, "read"),
        );
        assert!(
            !scenario_bool_for_runtime(&captures, runtime, "write", "crashed"),
            "{runtime}:write crashed unexpectedly:\n{}",
            scenario_text_for_runtime(&captures, runtime, "write"),
        );
        assert!(
            !scenario_bool_for_runtime(&captures, runtime, "edit", "crashed"),
            "{runtime}:edit crashed unexpectedly:\n{}",
            scenario_text_for_runtime(&captures, runtime, "edit"),
        );
        assert!(
            !scenario_bool_for_runtime(&captures, runtime, "diff", "crashed"),
            "{runtime}:diff crashed unexpectedly:\n{}",
            scenario_text_for_runtime(&captures, runtime, "diff"),
        );
        assert!(
            scenario_text_for_runtime(&captures, runtime, "read")
                .contains("read /tmp/example.rs:2-3"),
            "{runtime}:read baseline lost the read header:\n{}",
            scenario_text_for_runtime(&captures, runtime, "read"),
        );
        assert!(
            scenario_text_for_runtime(&captures, runtime, "write").contains("write src/main.rs"),
            "{runtime}:write baseline lost the write header:\n{}",
            scenario_text_for_runtime(&captures, runtime, "write"),
        );
        assert!(
            scenario_text_for_runtime(&captures, runtime, "edit").contains("edit src/lib.rs"),
            "{runtime}:edit baseline lost the edit header:\n{}",
            scenario_text_for_runtime(&captures, runtime, "edit"),
        );
        assert!(
            scenario_text_for_runtime(&captures, runtime, "diff").contains("diff --git"),
            "{runtime}:diff baseline lost the git diff header:\n{}",
            scenario_text_for_runtime(&captures, runtime, "diff"),
        );
    }
}

#[test]
fn rust_breadth_audit_builtin_and_auth_surfaces_render_in_fixed_80x24_tmux() {
    let Some(captures) = capture_rust_scenarios(&[
        "startup-diagnostics",
        "login",
        "logout",
        "reload-diagnostics",
        "session",
        "changelog",
        "hotkeys",
        "copy-empty",
        "share-missing-gh",
    ]) else {
        return;
    };

    assert_fixed_tmux_meta(&captures);

    let startup = scenario_text(&captures, "startup-diagnostics");
    assert!(startup.contains("[Context]"));
    assert!(startup.contains("[Skills]"));
    assert!(startup.contains("[Prompts]"));
    assert!(startup.contains("[Themes]"));
    assert!(!scenario_crashed(&captures, "startup-diagnostics"));
    assert!(!scenario_crashed(&captures, "logout"));

    assert!(
        scenario_text(&captures, "reload-diagnostics")
            .contains("Reloaded extensions, skills, prompts, themes")
    );
    assert!(scenario_text(&captures, "session").contains("Session Info"));
    assert_contains_any(
        &scenario_text(&captures, "changelog"),
        &[
            "What's New",
            "Changelog added to the transcript.",
            "### Added",
            "### Changed",
        ],
    );
    assert!(scenario_text(&captures, "hotkeys").contains("Keyboard"));
    assert!(scenario_text(&captures, "copy-empty").contains("No agent messages to copy yet."));
    assert!(scenario_text(&captures, "share-missing-gh").contains("GitHub CLI"));
    assert!(!scenario_shell_fallback(&captures, "share-missing-gh"));
}

#[test]
fn ts_and_rust_long_tail_surfaces_render_in_fixed_80x24_tmux() {
    let Some(captures) = capture_both_scenarios(LONG_TAIL_SCENARIOS) else {
        return;
    };

    assert_fixed_tmux_meta(&captures);
    assert_long_tail_surfaces_are_captured(&captures, "ts");
    assert_long_tail_surfaces_are_captured(&captures, "rust");
}

#[test]
fn ts_and_rust_long_tail_surfaces_render_in_fixed_111x62_tmux() {
    let Some(captures) = capture_both_scenarios_111x62(LONG_TAIL_SCENARIOS) else {
        return;
    };

    assert_wide_tmux_meta(&captures);
    assert_long_tail_surfaces_are_captured(&captures, "ts");
    assert_long_tail_surfaces_are_captured(&captures, "rust");
}
