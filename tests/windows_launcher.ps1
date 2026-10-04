param(
    [string]$Binary = (Join-Path $PSScriptRoot '..\target\release\claude-acc.exe')
)

$ErrorActionPreference = 'Stop'
if (Get-Variable -Name PSNativeCommandUseErrorActionPreference -ErrorAction SilentlyContinue) {
    $PSNativeCommandUseErrorActionPreference = $false
}

function Assert-True {
    param([bool]$Condition, [string]$Message)
    if (-not $Condition) { throw "ASSERTION FAILED: $Message" }
}

function Assert-Equal {
    param($Actual, $Expected, [string]$Message)
    if ($Actual -ne $Expected) {
        throw "ASSERTION FAILED: $Message`nexpected: $Expected`nactual:   $Actual"
    }
}

$Binary = [IO.Path]::GetFullPath($Binary)
Assert-True (Test-Path -LiteralPath $Binary -PathType Leaf) "release binary missing: $Binary"

$tempRoot = $env:RUNNER_TEMP
if ([string]::IsNullOrWhiteSpace($tempRoot)) { $tempRoot = $env:TEMP }
$root = Join-Path $tempRoot 'claude-acc R1 Δ secure launch'
$resolvedTempRoot = [IO.Path]::GetFullPath($tempRoot).TrimEnd([IO.Path]::DirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
$resolvedRoot = [IO.Path]::GetFullPath($root)
Assert-True $resolvedRoot.StartsWith($resolvedTempRoot, [StringComparison]::OrdinalIgnoreCase) 'test root escaped the runner temporary directory'
$homeDir = Join-Path $root 'profile home ü'
$trustedBin = Join-Path $root 'trusted bin Ω'
$workingDir = Join-Path $root 'working repo 漢字'
$captureDir = Join-Path $root 'captures'
$managerTarget = Join-Path $homeDir '.claude-switch'
$claudeTarget = Join-Path $homeDir '.claude'
$accountTarget = Join-Path $managerTarget 'accounts\personal1'
$account2Target = Join-Path $managerTarget 'accounts\personal2'
$profileRoot = [Environment]::GetFolderPath('UserProfile')
$managerLink = Join-Path $profileRoot '.claude-switch'
$claudeLink = Join-Path $profileRoot '.claude'
$accountDir = Join-Path $managerLink 'accounts\personal1'
$account2Dir = Join-Path $managerLink 'accounts\personal2'
$globalAnthropicDir = Join-Path $root 'global Anthropic profile Ω'
$globalAnthropicCredentials = Join-Path $globalAnthropicDir 'credentials'

$fixture = Join-Path $PSScriptRoot 'fixtures\fake_claude.rs'
$fakeClaude = Join-Path $trustedBin 'claude.exe'
$decoyClaude = Join-Path $workingDir 'claude.exe'
$decoyShellDir = Join-Path $root 'decoy shell'
$decoyShell = Join-Path $decoyShellDir 'cmd.exe'

$denylist = @(
    'ANTHROPIC_API_KEY', 'ANTHROPIC_AUTH_TOKEN', 'CLAUDE_CODE_OAUTH_TOKEN',
    'AWS_BEARER_TOKEN_BEDROCK', 'ANTHROPIC_AWS_API_KEY',
    'ANTHROPIC_FOUNDRY_API_KEY', 'ANTHROPIC_FOUNDRY_AUTH_TOKEN',
    'CLAUDE_CODE_USE_ANTHROPIC_AWS', 'CLAUDE_CODE_USE_BEDROCK',
    'CLAUDE_CODE_USE_FOUNDRY', 'CLAUDE_CODE_USE_MANTLE',
    'CLAUDE_CODE_USE_VERTEX', 'CLAUDE_CODE_PROVIDER_MANAGED_BY_HOST',
    'ANTHROPIC_PROFILE',
    'ANTHROPIC_FEDERATION_RULE_ID', 'ANTHROPIC_ORGANIZATION_ID',
    'ANTHROPIC_WORKSPACE_ID', 'CLAUDE_CODE_OAUTH_REFRESH_TOKEN',
    'CLAUDE_CODE_OAUTH_SCOPES', 'ANTHROPIC_BASE_URL',
    'ANTHROPIC_AWS_BASE_URL', 'ANTHROPIC_AWS_WORKSPACE_ID',
    'ANTHROPIC_BEDROCK_BASE_URL', 'ANTHROPIC_BEDROCK_MANTLE_BASE_URL',
    'ANTHROPIC_BEDROCK_REGION_PREFIX', 'ANTHROPIC_CUSTOM_HEADERS',
    'ANTHROPIC_FOUNDRY_BASE_URL', 'ANTHROPIC_FOUNDRY_RESOURCE',
    'ANTHROPIC_VERTEX_BASE_URL', 'ANTHROPIC_VERTEX_PROJECT_ID',
    'CLAUDE_CODE_SKIP_ANTHROPIC_AWS_AUTH', 'CLAUDE_CODE_SKIP_BEDROCK_AUTH',
    'CLAUDE_CODE_SKIP_FOUNDRY_AUTH', 'CLAUDE_CODE_SKIP_MANTLE_AUTH',
    'CLAUDE_CODE_SKIP_VERTEX_AUTH',
    'CLAUDE_SECURESTORAGE_CONFIG_DIR'
)
$secretSentinel = 'R1_SECRET_VALUE_MUST_NEVER_BE_CAPTURED_7f93'
$profileSecretSentinel = 'R1_PROFILE_SECRET_MUST_NEVER_BE_CAPTURED_81ab'
$savedEnvironment = @{}
$savedPath = $env:PATH
$savedPathExt = $env:PATHEXT
$savedComSpec = $env:ComSpec
$savedConfig = $env:CLAUDE_CONFIG_DIR
$savedAnthropicConfig = $env:ANTHROPIC_CONFIG_DIR
$environmentSaved = $false

try {
    if (Test-Path -LiteralPath $root) { Remove-Item -LiteralPath $root -Recurse -Force }
    if ((Test-Path -LiteralPath $managerLink) -or (Test-Path -LiteralPath $claudeLink)) {
        throw 'Refusing to touch an existing real ~/.claude-switch or ~/.claude directory.'
    }
    New-Item -ItemType Directory -Force -Path $homeDir, $trustedBin, $workingDir, $captureDir, $accountTarget, $account2Target, $claudeTarget, $decoyShellDir, $globalAnthropicDir, $globalAnthropicCredentials | Out-Null
    New-Item -ItemType Junction -Path $managerLink -Target $managerTarget | Out-Null
    New-Item -ItemType Junction -Path $claudeLink -Target $claudeTarget | Out-Null
    Set-Content -LiteralPath (Join-Path $globalAnthropicDir 'active_config') -Value 'global-federated-profile' -Encoding Ascii
    Set-Content -LiteralPath (Join-Path $globalAnthropicCredentials 'global-federated-profile.json') -Value $profileSecretSentinel -Encoding Ascii

    & rustc $fixture '-o' $fakeClaude
    Assert-Equal $LASTEXITCODE 0 'fake Claude compilation'
    & rustc '--cfg' 'decoy' $fixture '-o' $decoyClaude
    Assert-Equal $LASTEXITCODE 0 'cwd decoy compilation'
    & rustc '--cfg' 'decoy' $fixture '-o' $decoyShell
    Assert-Equal $LASTEXITCODE 0 'ComSpec decoy compilation'

    foreach ($name in $denylist) {
        $savedEnvironment[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
    }
    $environmentSaved = $true
    foreach ($name in $denylist) {
        [Environment]::SetEnvironmentVariable($name, $secretSentinel, 'Process')
    }
    $env:PATH = "$trustedBin;$savedPath"
    $env:PATHEXT = '.EXE;.COM;.CMD;.BAT'
    $env:ComSpec = $decoyShell
    $env:CLAUDE_CONFIG_DIR = (Join-Path $root 'inherited wrong profile')
    $env:ANTHROPIC_CONFIG_DIR = $globalAnthropicDir

    function Invoke-Manager {
        param([string]$Name, [string[]]$Arguments, [int]$ChildExit = 0)
        $capture = Join-Path $captureDir "$Name.json"
        Remove-Item -LiteralPath $capture -Force -ErrorAction SilentlyContinue
        $env:CLAUDE_ACC_TEST_CAPTURE = $capture
        $env:CLAUDE_ACC_TEST_EXIT_CODE = [string]$ChildExit
        Push-Location -LiteralPath $workingDir
        try {
            & $Binary @Arguments | Out-Null
            $code = $LASTEXITCODE
        } finally {
            Pop-Location
        }
        $record = $null
        if (Test-Path -LiteralPath $capture) {
            $raw = Get-Content -LiteralPath $capture -Raw
            Assert-True (-not $raw.Contains($secretSentinel)) "$Name capture leaked a secret value"
            Assert-True (-not $raw.Contains($profileSecretSentinel)) "$Name capture leaked Anthropic profile credential content"
            $record = $raw | ConvertFrom-Json
        }
        [pscustomobject]@{ Code = $code; Record = $record; Capture = $capture }
    }

    $ordinary = Invoke-Manager 'ordinary' @('run', 'personal1', 'a', 'b c', '--flag=value')
    Assert-Equal $ordinary.Code 0 'native direct launch exit code'
    Assert-True ($null -ne $ordinary.Record) 'native fake was not executed'
    Assert-Equal $ordinary.Record.argv.Count 3 'ordinary argv count'
    Assert-Equal $ordinary.Record.argv[0] 'a' 'argv[0]'
    Assert-Equal $ordinary.Record.argv[1] 'b c' 'argv[1]'
    Assert-Equal $ordinary.Record.argv[2] '--flag=value' 'argv[2]'
    Assert-Equal ([IO.Path]::GetFullPath($ordinary.Record.config_dir)) ([IO.Path]::GetFullPath($accountDir)) 'named CLAUDE_CONFIG_DIR'
    $personal1AnthropicDir = Join-Path $accountDir '.anthropic'
    Assert-Equal ([IO.Path]::GetFullPath($ordinary.Record.anthropic_config_dir)) ([IO.Path]::GetFullPath($personal1AnthropicDir)) 'personal1 ANTHROPIC_CONFIG_DIR'
    Assert-True ([IO.Path]::GetFullPath($ordinary.Record.anthropic_config_dir) -ne [IO.Path]::GetFullPath($globalAnthropicDir)) 'personal1 inherited global ANTHROPIC_CONFIG_DIR'
    Assert-Equal ([IO.Path]::GetFullPath($ordinary.Record.cwd)) ([IO.Path]::GetFullPath($workingDir)) 'child cwd'
    Assert-Equal $ordinary.Record.denylisted_present.Count 0 'denylisted variables reached child'

    $second = Invoke-Manager 'second-account' @('run', 'personal2')
    Assert-Equal $second.Code 0 'second named profile exit'
    Assert-Equal ([IO.Path]::GetFullPath($second.Record.config_dir)) ([IO.Path]::GetFullPath($account2Dir)) 'personal2 CLAUDE_CONFIG_DIR'
    $personal2AnthropicDir = Join-Path $account2Dir '.anthropic'
    Assert-Equal ([IO.Path]::GetFullPath($second.Record.anthropic_config_dir)) ([IO.Path]::GetFullPath($personal2AnthropicDir)) 'personal2 ANTHROPIC_CONFIG_DIR'
    Assert-True ([IO.Path]::GetFullPath($second.Record.anthropic_config_dir) -ne [IO.Path]::GetFullPath($ordinary.Record.anthropic_config_dir)) 'named accounts shared ANTHROPIC_CONFIG_DIR'

    $bypass = Invoke-Manager 'bypass' @('run', 'personal1', '--dangerously-skip-permissions')
    Assert-Equal $bypass.Code 0 'local bypass passthrough exit'
    Assert-Equal $bypass.Record.argv.Count 1 'local bypass argv count'
    Assert-Equal $bypass.Record.argv[0] '--dangerously-skip-permissions' 'local bypass changed'

    $default = Invoke-Manager 'default' @('run', 'default')
    Assert-Equal $default.Code 0 'default profile exit'
    Assert-True ($null -eq $default.Record.config_dir) 'default inherited CLAUDE_CONFIG_DIR'
    Assert-Equal ([IO.Path]::GetFullPath($default.Record.anthropic_config_dir)) ([IO.Path]::GetFullPath($globalAnthropicDir)) 'default did not preserve inherited ANTHROPIC_CONFIG_DIR'

    $cloudTask = 'Continue Δ with "quotes", 50% & exact spacing'
    $cloud = Invoke-Manager 'cloud-named' @('cloud', 'personal1', $cloudTask)
    Assert-Equal $cloud.Code 0 'cloud named exit'
    Assert-Equal $cloud.Record.argv.Count 4 'cloud argv count'
    Assert-Equal $cloud.Record.argv[0] '--cloud' 'cloud argv[0]'
    Assert-Equal $cloud.Record.argv[1] $cloudTask 'cloud task changed'
    Assert-Equal $cloud.Record.argv[2] '--permission-mode' 'cloud argv[2]'
    Assert-Equal $cloud.Record.argv[3] 'auto' 'cloud permission mode'
    Assert-Equal ([IO.Path]::GetFullPath($cloud.Record.config_dir)) ([IO.Path]::GetFullPath($accountDir)) 'cloud named CLAUDE_CONFIG_DIR'
    Assert-Equal ([IO.Path]::GetFullPath($cloud.Record.anthropic_config_dir)) ([IO.Path]::GetFullPath($personal1AnthropicDir)) 'cloud named ANTHROPIC_CONFIG_DIR'
    Assert-Equal $cloud.Record.denylisted_present.Count 0 'cloud named environment scrub'

    $cloudDefault = Invoke-Manager 'cloud-default' @('cloud', 'default', 'Default cloud task')
    Assert-Equal $cloudDefault.Code 0 'cloud default exit'
    Assert-Equal ($cloudDefault.Record.argv -join ',') '--cloud,Default cloud task,--permission-mode,auto' 'cloud default argv'
    Assert-True ($null -eq $cloudDefault.Record.config_dir) 'cloud default CLAUDE_CONFIG_DIR'
    Assert-Equal ([IO.Path]::GetFullPath($cloudDefault.Record.anthropic_config_dir)) ([IO.Path]::GetFullPath($globalAnthropicDir)) 'cloud default ANTHROPIC_CONFIG_DIR'

    $teleport = Invoke-Manager 'teleport-named' @('teleport', 'personal1', 'session_123')
    Assert-Equal $teleport.Code 0 'teleport named exit'
    Assert-Equal $teleport.Record.argv.Count 2 'teleport argv count'
    Assert-Equal $teleport.Record.argv[0] '--teleport' 'teleport argv[0]'
    Assert-Equal $teleport.Record.argv[1] 'session_123' 'teleport session changed'
    Assert-Equal ([IO.Path]::GetFullPath($teleport.Record.config_dir)) ([IO.Path]::GetFullPath($accountDir)) 'teleport named CLAUDE_CONFIG_DIR'
    Assert-Equal ([IO.Path]::GetFullPath($teleport.Record.anthropic_config_dir)) ([IO.Path]::GetFullPath($personal1AnthropicDir)) 'teleport named ANTHROPIC_CONFIG_DIR'
    Assert-Equal $teleport.Record.denylisted_present.Count 0 'teleport named environment scrub'

    $teleportDefault = Invoke-Manager 'teleport-default' @('teleport', 'default', 'cse_opaque')
    Assert-Equal $teleportDefault.Code 0 'teleport default exit'
    Assert-Equal ($teleportDefault.Record.argv -join ',') '--teleport,cse_opaque' 'teleport default argv'
    Assert-True ($null -eq $teleportDefault.Record.config_dir) 'teleport default CLAUDE_CONFIG_DIR'
    Assert-Equal ([IO.Path]::GetFullPath($teleportDefault.Record.anthropic_config_dir)) ([IO.Path]::GetFullPath($globalAnthropicDir)) 'teleport default ANTHROPIC_CONFIG_DIR'

    $cloudBypass = Invoke-Manager 'cloud-bypass-rejected' @('cloud', 'personal1', 'task', '--dangerously-skip-permissions')
    Assert-Equal $cloudBypass.Code 2 'cloud bypass must fail Clap validation'
    Assert-True ($null -eq $cloudBypass.Record) 'cloud bypass unexpectedly launched Claude'

    $cloudLocator = Invoke-Manager 'cloud-locator-rejected' @('cloud', 'personal1', 'session_012345')
    Assert-Equal $cloudLocator.Code 1 'cloud locator must fail manager validation'
    Assert-True ($null -eq $cloudLocator.Record) 'cloud locator unexpectedly launched Claude'

    foreach ($case in @(
        @{ Name = 'cloud-missing-account'; Args = @('cloud', 'missing', 'task') },
        @{ Name = 'teleport-missing-account'; Args = @('teleport', 'missing', 'session_123') }
    )) {
        $missing = Invoke-Manager $case.Name $case.Args
        Assert-Equal $missing.Code 1 "$($case.Name) exit"
        Assert-True ($null -eq $missing.Record) "$($case.Name) unexpectedly launched Claude"
    }

    $cloudExit = Invoke-Manager 'cloud-exit-42' @('cloud', 'personal1', 'exit propagation') 42
    Assert-Equal $cloudExit.Code 42 'cloud child exit propagation'
    $teleportExit = Invoke-Manager 'teleport-exit-7' @('teleport', 'personal1', 'session_exit') 7
    Assert-Equal $teleportExit.Code 7 'teleport child exit propagation'

    $quoteArgs = @('run', 'personal1', 'a"b', '50%literal', 'a&b', 'C:\tail\')
    $quotes = Invoke-Manager 'quote-relevant' $quoteArgs
    Assert-Equal $quotes.Code 0 'native quote-relevant arguments'
    Assert-Equal $quotes.Record.argv[0] 'a"b' 'embedded quote changed'
    Assert-Equal $quotes.Record.argv[1] '50%literal' 'percent changed'
    Assert-Equal $quotes.Record.argv[2] 'a&b' 'ampersand changed'
    Assert-Equal $quotes.Record.argv[3] 'C:\tail\' 'trailing backslash changed'

    foreach ($exitCode in @(0, 1, 2, 42)) {
        $result = Invoke-Manager "exit-$exitCode" @('run', 'personal1') $exitCode
        Assert-Equal $result.Code $exitCode "child exit code $exitCode"
    }

    $add = Invoke-Manager 'add' @('add', 'added-by-fake')
    Assert-Equal $add.Code 0 'add through fake Claude'
    Assert-Equal ($add.Record.argv -join ',') 'auth,login' 'add login argv'
    Assert-True $add.Record.config_dir.EndsWith('added-by-fake') 'add config directory'
    Assert-Equal ([IO.Path]::GetFullPath($add.Record.anthropic_config_dir)) ([IO.Path]::GetFullPath((Join-Path $add.Record.config_dir '.anthropic'))) 'add ANTHROPIC_CONFIG_DIR'
    Assert-Equal $add.Record.denylisted_present.Count 0 'add environment scrub'

    $login = Invoke-Manager 'login' @('login', 'personal1')
    Assert-Equal $login.Code 0 'login through fake Claude'
    Assert-Equal ($login.Record.argv -join ',') 'auth,login' 'login argv'
    Assert-Equal ([IO.Path]::GetFullPath($login.Record.anthropic_config_dir)) ([IO.Path]::GetFullPath($personal1AnthropicDir)) 'login ANTHROPIC_CONFIG_DIR'
    Assert-Equal $login.Record.denylisted_present.Count 0 'login environment scrub'

    Remove-Item -LiteralPath $fakeClaude -Force
    Set-Content -LiteralPath (Join-Path $trustedBin 'claude.cmd') -Value '@exit /b 0' -Encoding Ascii
    $unsafe = Invoke-Manager 'unsafe-batch' @('run', 'personal1', 'bad"argument')
    Assert-Equal $unsafe.Code 1 'unsafe batch argument must fail explicitly'
    Assert-True ($null -eq $unsafe.Record) 'unknown batch fallback unexpectedly executed'

    Assert-True ([IO.Path]::GetPathRoot($root) -match '^[A-Za-z]:\\$') 'test path is not a drive-letter path'
    Assert-True $root.Contains(' ') 'test path lacks spaces'
    Assert-True $root.Contains('Δ') 'test path lacks Unicode'
    Write-Host 'Windows secure launcher acceptance tests passed.'
} finally {
    if ($environmentSaved) {
        $env:PATH = $savedPath
        $env:PATHEXT = $savedPathExt
        $env:ComSpec = $savedComSpec
        $env:CLAUDE_CONFIG_DIR = $savedConfig
        $env:ANTHROPIC_CONFIG_DIR = $savedAnthropicConfig
        Remove-Item Env:CLAUDE_ACC_TEST_CAPTURE -ErrorAction SilentlyContinue
        Remove-Item Env:CLAUDE_ACC_TEST_EXIT_CODE -ErrorAction SilentlyContinue
        foreach ($name in $denylist) {
            [Environment]::SetEnvironmentVariable($name, $savedEnvironment[$name], 'Process')
        }
    }
    if (Test-Path -LiteralPath $managerLink) { Remove-Item -LiteralPath $managerLink -Force }
    if (Test-Path -LiteralPath $claudeLink) { Remove-Item -LiteralPath $claudeLink -Force }
    if (Test-Path -LiteralPath $root) { Remove-Item -LiteralPath $root -Recurse -Force }
}

exit 0
