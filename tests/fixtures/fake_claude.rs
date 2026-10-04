#![allow(unexpected_cfgs)]

#[cfg(not(decoy))]
use std::{env, fs, path::PathBuf};
use std::process;

#[cfg(not(decoy))]
const DENYLIST: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "AWS_BEARER_TOKEN_BEDROCK",
    "ANTHROPIC_AWS_API_KEY",
    "ANTHROPIC_FOUNDRY_API_KEY",
    "ANTHROPIC_FOUNDRY_AUTH_TOKEN",
    "CLAUDE_CODE_USE_ANTHROPIC_AWS",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_FOUNDRY",
    "CLAUDE_CODE_USE_MANTLE",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_PROVIDER_MANAGED_BY_HOST",
    "ANTHROPIC_PROFILE",
    "ANTHROPIC_FEDERATION_RULE_ID",
    "ANTHROPIC_ORGANIZATION_ID",
    "ANTHROPIC_WORKSPACE_ID",
    "CLAUDE_CODE_OAUTH_REFRESH_TOKEN",
    "CLAUDE_CODE_OAUTH_SCOPES",
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_AWS_BASE_URL",
    "ANTHROPIC_AWS_WORKSPACE_ID",
    "ANTHROPIC_BEDROCK_BASE_URL",
    "ANTHROPIC_BEDROCK_MANTLE_BASE_URL",
    "ANTHROPIC_BEDROCK_REGION_PREFIX",
    "ANTHROPIC_CUSTOM_HEADERS",
    "ANTHROPIC_FOUNDRY_BASE_URL",
    "ANTHROPIC_FOUNDRY_RESOURCE",
    "ANTHROPIC_VERTEX_BASE_URL",
    "ANTHROPIC_VERTEX_PROJECT_ID",
    "CLAUDE_CODE_SKIP_ANTHROPIC_AWS_AUTH",
    "CLAUDE_CODE_SKIP_BEDROCK_AUTH",
    "CLAUDE_CODE_SKIP_FOUNDRY_AUTH",
    "CLAUDE_CODE_SKIP_MANTLE_AUTH",
    "CLAUDE_CODE_SKIP_VERTEX_AUTH",
    "CLAUDE_SECURESTORAGE_CONFIG_DIR",
];

#[cfg(decoy)]
fn main() {
    process::exit(97);
}

#[cfg(not(decoy))]
fn main() {
    let capture = env::var_os("CLAUDE_ACC_TEST_CAPTURE")
        .map(PathBuf::from)
        .expect("CLAUDE_ACC_TEST_CAPTURE is required");
    let args: Vec<String> = env::args_os()
        .skip(1)
        .map(|value| value.to_string_lossy().into_owned())
        .collect();
    let config_dir = env::var_os("CLAUDE_CONFIG_DIR")
        .map(|value| value.to_string_lossy().into_owned());
    let cwd = env::current_dir()
        .expect("current directory")
        .to_string_lossy()
        .into_owned();
    let present: Vec<&str> = DENYLIST
        .iter()
        .copied()
        .filter(|name| env::var_os(name).is_some())
        .collect();

    // Credential values are never read. The capture contains names only for
    // any denylisted variables that unexpectedly survived.
    let json = format!(
        "{{\"argv\":{},\"config_dir\":{},\"cwd\":{},\"denylisted_present\":{}}}",
        json_array(args.iter().map(String::as_str)),
        config_dir
            .as_deref()
            .map(json_string)
            .unwrap_or_else(|| "null".to_string()),
        json_string(&cwd),
        json_array(present),
    );
    fs::write(capture, json).expect("write capture");

    let exit_code = env::var("CLAUDE_ACC_TEST_EXIT_CODE")
        .ok()
        .and_then(|value| value.parse::<i32>().ok())
        .unwrap_or(0);
    process::exit(exit_code);
}

#[cfg(not(decoy))]
fn json_array<'a>(values: impl IntoIterator<Item = &'a str>) -> String {
    let values: Vec<String> = values.into_iter().map(json_string).collect();
    format!("[{}]", values.join(","))
}

#[cfg(not(decoy))]
fn json_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('"');
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            c if c < ' ' => escaped.push_str(&format!("\\u{:04x}", c as u32)),
            c => escaped.push(c),
        }
    }
    escaped.push('"');
    escaped
}
