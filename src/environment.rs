// Inherited Claude authentication, provider-selection, and routing variables
// must not override the account selected by CLAUDE_CONFIG_DIR.
// Evidence: https://code.claude.com/docs/en/env-vars and
// https://code.claude.com/docs/en/iam#authentication-precedence.
// CLAUDE_SECURESTORAGE_CONFIG_DIR is also evidenced by installed Claude 2.1.220.
// Generic cloud variables, PATH, HOME, and USERPROFILE remain inherited.
pub const CLAUDE_AUTH_ENV_VARS: &[&str] = &[
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

pub fn strip_claude_auth_env(cmd: &mut std::process::Command) {
    for var in CLAUDE_AUTH_ENV_VARS {
        cmd.env_remove(var);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_claude_auth_env_removes_every_listed_var() {
        let mut cmd = std::process::Command::new("true");
        for var in CLAUDE_AUTH_ENV_VARS {
            cmd.env(var, "dummy-secret-never-spawned");
        }
        strip_claude_auth_env(&mut cmd);
        for var in CLAUDE_AUTH_ENV_VARS {
            let removed = cmd
                .get_envs()
                .find(|(key, _)| *key == std::ffi::OsStr::new(var));
            assert_eq!(removed, Some((std::ffi::OsStr::new(*var), None)));
        }
    }

    #[test]
    fn generic_cloud_and_host_environment_is_not_scrubbed() {
        for var in [
            "AWS_PROFILE",
            "AWS_ACCESS_KEY_ID",
            "GOOGLE_APPLICATION_CREDENTIALS",
            "AZURE_CLIENT_ID",
            "PATH",
            "HOME",
            "USERPROFILE",
        ] {
            assert!(!CLAUDE_AUTH_ENV_VARS.contains(&var), "{var} is too broad");
        }
    }
}
