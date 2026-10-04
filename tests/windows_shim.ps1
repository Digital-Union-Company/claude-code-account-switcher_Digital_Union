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
$root = Join-Path $tempRoot "claude-acc R3A O'Brien native shim Δ"
$resolvedTemp = [IO.Path]::GetFullPath($tempRoot).TrimEnd([IO.Path]::DirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
$resolvedRoot = [IO.Path]::GetFullPath($root)
Assert-True $resolvedRoot.StartsWith($resolvedTemp, [StringComparison]::OrdinalIgnoreCase) 'test root escaped runner temp'

$profileRoot = [Environment]::GetFolderPath('UserProfile')
$managerLink = Join-Path $profileRoot '.claude-switch'
$managerTarget = Join-Path $root 'isolated manager root ü'
$managerBin = Join-Path $managerTarget 'bin'
$accounts = Join-Path $managerTarget 'accounts'
$personal1 = Join-Path $accounts 'personal1'
$personal2 = Join-Path $accounts 'personal2'
$logicalAccounts = Join-Path $managerLink 'accounts'
$logicalPersonal1 = Join-Path $logicalAccounts 'personal1'
$logicalPersonal2 = Join-Path $logicalAccounts 'personal2'
$trustedBin = Join-Path $root 'trusted real Claude Ω'
$captureDir = Join-Path $root 'captures'
$linkedRoot = Join-Path $root 'linked repo 漢字'
$nestedRoot = Join-Path $linkedRoot 'nearest account'
$nestedChild = Join-Path $nestedRoot 'child'
$unlinked = Join-Path $root 'unlinked default'
$explicitDefault = Join-Path $root 'explicit default'
$shim = Join-Path $managerBin 'claude.exe'
$manager = Join-Path $managerBin 'claude-acc.exe'
$fakeClaude = Join-Path $trustedBin 'claude.exe'
$decoyClaude = Join-Path $linkedRoot 'claude.exe'
$fixture = Join-Path $PSScriptRoot 'fixtures\fake_claude.rs'

$denylist = @(
    'ANTHROPIC_API_KEY', 'ANTHROPIC_AUTH_TOKEN', 'CLAUDE_CODE_OAUTH_TOKEN',
    'AWS_BEARER_TOKEN_BEDROCK', 'ANTHROPIC_AWS_API_KEY',
    'ANTHROPIC_FOUNDRY_API_KEY', 'ANTHROPIC_FOUNDRY_AUTH_TOKEN',
    'CLAUDE_CODE_USE_ANTHROPIC_AWS', 'CLAUDE_CODE_USE_BEDROCK',
    'CLAUDE_CODE_USE_FOUNDRY', 'CLAUDE_CODE_USE_MANTLE',
    'CLAUDE_CODE_USE_VERTEX', 'CLAUDE_CODE_PROVIDER_MANAGED_BY_HOST',
    'ANTHROPIC_PROFILE', 'ANTHROPIC_FEDERATION_RULE_ID',
    'ANTHROPIC_ORGANIZATION_ID', 'ANTHROPIC_WORKSPACE_ID',
    'CLAUDE_CODE_OAUTH_REFRESH_TOKEN', 'CLAUDE_CODE_OAUTH_SCOPES',
    'ANTHROPIC_BASE_URL', 'ANTHROPIC_AWS_BASE_URL',
    'ANTHROPIC_AWS_WORKSPACE_ID', 'ANTHROPIC_BEDROCK_BASE_URL',
    'ANTHROPIC_BEDROCK_MANTLE_BASE_URL', 'ANTHROPIC_BEDROCK_REGION_PREFIX',
    'ANTHROPIC_CUSTOM_HEADERS', 'ANTHROPIC_FOUNDRY_BASE_URL',
    'ANTHROPIC_FOUNDRY_RESOURCE', 'ANTHROPIC_VERTEX_BASE_URL',
    'ANTHROPIC_VERTEX_PROJECT_ID', 'CLAUDE_CODE_SKIP_ANTHROPIC_AWS_AUTH',
    'CLAUDE_CODE_SKIP_BEDROCK_AUTH', 'CLAUDE_CODE_SKIP_FOUNDRY_AUTH',
    'CLAUDE_CODE_SKIP_MANTLE_AUTH', 'CLAUDE_CODE_SKIP_VERTEX_AUTH',
    'CLAUDE_SECURESTORAGE_CONFIG_DIR'
)
$secretSentinel = 'R3A_SECRET_MUST_NOT_BE_CAPTURED_f34b'
$savedEnvironment = @{}
$savedPath = $env:PATH
$savedPathExt = $env:PATHEXT
$savedConfig = $env:CLAUDE_CONFIG_DIR
$savedAnthropicConfig = $env:ANTHROPIC_CONFIG_DIR
$environmentSaved = $false

try {
    if (Test-Path -LiteralPath $root) { Remove-Item -LiteralPath $root -Recurse -Force }
    if (Test-Path -LiteralPath $managerLink) {
        throw 'Refusing to touch an existing real ~/.claude-switch directory.'
    }
    New-Item -ItemType Directory -Force -Path $managerBin, $personal1, $personal2, $trustedBin, $captureDir, $linkedRoot, $nestedChild, $unlinked, $explicitDefault | Out-Null
    New-Item -ItemType Junction -Path $managerLink -Target $managerTarget | Out-Null
    Copy-Item -LiteralPath $Binary -Destination $manager -Force
    Copy-Item -LiteralPath $Binary -Destination $shim -Force

    Set-Content -LiteralPath (Join-Path $managerTarget 'config') -Value 'default=' -Encoding utf8NoBOM
    @(
        "$linkedRoot=personal1"
        "$nestedRoot=personal2"
        "$explicitDefault=default"
    ) | Set-Content -LiteralPath (Join-Path $managerTarget 'links') -Encoding utf8NoBOM

    & rustc $fixture '-o' $fakeClaude
    Assert-Equal $LASTEXITCODE 0 'fake real Claude compilation'
    & rustc '--cfg' 'decoy' $fixture '-o' $decoyClaude
    Assert-Equal $LASTEXITCODE 0 'cwd decoy compilation'

    $initOutput = (& $manager init pwsh) -join "`n"
    $tokens = $null
    $parseErrors = $null
    [System.Management.Automation.Language.Parser]::ParseInput($initOutput, [ref]$tokens, [ref]$parseErrors) | Out-Null
    Assert-Equal $parseErrors.Count 0 'init pwsh output with apostrophe path must parse'
    Assert-True $initOutput.Contains("O''Brien") 'init pwsh did not escape apostrophe path'

    Push-Location -LiteralPath $linkedRoot
    try {
        $activateOutput = (& $manager activate --shell powershell) -join "`n"
    } finally {
        Pop-Location
    }
    $tokens = $null
    $parseErrors = $null
    [System.Management.Automation.Language.Parser]::ParseInput($activateOutput, [ref]$tokens, [ref]$parseErrors) | Out-Null
    Assert-Equal $parseErrors.Count 0 'PowerShell activation output must parse'
    # AppConfig intentionally retains the logical ~/.claude-switch junction
    # path; resolving its apostrophe-bearing target would be R3B path
    # normalization. The Rust activation unit test supplies an apostrophe in
    # the account path directly. Here, prove the end-to-end command emits the
    # exact logical account assignment and remains valid PowerShell.
    $expectedActivationPath = (Join-Path $managerLink 'accounts\personal1').Replace("'", "''")
    Assert-True $activateOutput.Contains("'$expectedActivationPath'") 'PowerShell activation emitted the wrong account path'

    foreach ($name in $denylist) {
        $savedEnvironment[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
    }
    $environmentSaved = $true
    foreach ($name in $denylist) {
        [Environment]::SetEnvironmentVariable($name, $secretSentinel, 'Process')
    }
    $env:PATH = "$managerBin;$trustedBin;$savedPath"
    $env:PATHEXT = '.EXE;.COM;.CMD;.BAT'
    $env:CLAUDE_CONFIG_DIR = (Join-Path $root 'stale inherited Claude profile')
    $env:ANTHROPIC_CONFIG_DIR = (Join-Path $root 'global Anthropic profile')

    function Invoke-Shim {
        param([string]$Name, [string]$WorkingDirectory, [string[]]$Arguments, [int]$ChildExit = 0)
        $capture = Join-Path $captureDir "$Name.json"
        Remove-Item -LiteralPath $capture -Force -ErrorAction SilentlyContinue
        $env:CLAUDE_ACC_TEST_CAPTURE = $capture
        $env:CLAUDE_ACC_TEST_EXIT_CODE = [string]$ChildExit
        Push-Location -LiteralPath $WorkingDirectory
        try {
            & $shim @Arguments | Out-Null
            $code = $LASTEXITCODE
        } finally {
            Pop-Location
        }
        Assert-True (Test-Path -LiteralPath $capture -PathType Leaf) "$Name did not invoke fake real Claude"
        $raw = Get-Content -LiteralPath $capture -Raw
        Assert-True (-not $raw.Contains($secretSentinel)) "$Name leaked a secret value"
        [pscustomobject]@{
            Code = $code
            Record = ($raw | ConvertFrom-Json)
        }
    }

    $ordinary = Invoke-Shim 'linked-ordinary' $linkedRoot @('a', 'b c')
    Assert-Equal $ordinary.Code 0 'linked launch exit'
    Assert-Equal ($ordinary.Record.argv -join ',') 'a,b c' 'linked argv'
    Assert-Equal ([IO.Path]::GetFullPath($ordinary.Record.config_dir)) ([IO.Path]::GetFullPath($logicalPersonal1)) 'linked CLAUDE_CONFIG_DIR'
    Assert-Equal ([IO.Path]::GetFullPath($ordinary.Record.anthropic_config_dir)) ([IO.Path]::GetFullPath((Join-Path $logicalPersonal1 '.anthropic'))) 'linked ANTHROPIC_CONFIG_DIR'
    Assert-Equal $ordinary.Record.denylisted_present.Count 0 'linked environment scrub'

    $empty = Invoke-Shim 'empty-argv' $linkedRoot @()
    Assert-Equal $empty.Record.argv.Count 0 'empty Claude argv changed'

    # CI/stdout capture is non-interactive, so resume must never wait for the
    # manager prompt and must reach Claude unchanged.
    $resume = Invoke-Shim 'nonterminal-resume' $linkedRoot @('--resume', 'abc')
    Assert-Equal ($resume.Record.argv -join ',') '--resume,abc' 'non-terminal resume changed or blocked'
    $bareResume = Invoke-Shim 'bare-resume' $linkedRoot @('--resume')
    Assert-Equal ($bareResume.Record.argv -join ',') '--resume' 'bare resume picker changed'

    $nested = Invoke-Shim 'nested-nearest' $nestedChild @('--version')
    Assert-Equal ([IO.Path]::GetFullPath($nested.Record.config_dir)) ([IO.Path]::GetFullPath($logicalPersonal2)) 'nearest ancestor did not win'

    foreach ($case in @(
        @{ Name = 'unlinked-default'; Cwd = $unlinked },
        @{ Name = 'explicit-default'; Cwd = $explicitDefault }
    )) {
        $default = Invoke-Shim $case.Name $case.Cwd @('--version')
        Assert-True ($null -eq $default.Record.config_dir) "$($case.Name) retained CLAUDE_CONFIG_DIR"
        Assert-Equal ([IO.Path]::GetFullPath($default.Record.anthropic_config_dir)) ([IO.Path]::GetFullPath($env:ANTHROPIC_CONFIG_DIR)) "$($case.Name) changed upstream Anthropic profile semantics"
    }

    $specialArgs = @('quote"inside', 'Unicode-Ж-漢字', '50%literal', 'a&b', 'C:\tail\')
    $special = Invoke-Shim 'native-arguments' $linkedRoot $specialArgs
    Assert-Equal $special.Record.argv.Count $specialArgs.Count 'special argv count'
    for ($index = 0; $index -lt $specialArgs.Count; $index++) {
        Assert-Equal $special.Record.argv[$index] $specialArgs[$index] "special argv[$index]"
    }

    $bypass = Invoke-Shim 'bypass' $linkedRoot @('--dangerously-skip-permissions')
    Assert-Equal ($bypass.Record.argv -join ',') '--dangerously-skip-permissions' 'bypass changed'
    $cloud = Invoke-Shim 'cloud-passthrough' $linkedRoot @('--cloud', 'task')
    Assert-Equal ($cloud.Record.argv -join ',') '--cloud,task' 'shim reinterpreted cloud'
    $teleport = Invoke-Shim 'teleport-passthrough' $linkedRoot @('--teleport', 'session_123')
    Assert-Equal ($teleport.Record.argv -join ',') '--teleport,session_123' 'shim reinterpreted teleport'

    foreach ($exitCode in @(0, 1, 2, 42)) {
        $result = Invoke-Shim "exit-$exitCode" $linkedRoot @('--version') $exitCode
        Assert-Equal $result.Code $exitCode "child exit $exitCode"
    }

    # Reaching the trusted fake proves all three security properties together:
    # cwd decoy did not win, manager-bin claude.exe was excluded, and the shim
    # did not recursively launch itself.
    Assert-Equal ([IO.Path]::GetFullPath($ordinary.Record.cwd)) ([IO.Path]::GetFullPath($linkedRoot)) 'child cwd changed'
    Write-Host 'Windows native shim acceptance tests passed.'
} finally {
    if ($environmentSaved) {
        $env:PATH = $savedPath
        $env:PATHEXT = $savedPathExt
        $env:CLAUDE_CONFIG_DIR = $savedConfig
        $env:ANTHROPIC_CONFIG_DIR = $savedAnthropicConfig
        Remove-Item Env:CLAUDE_ACC_TEST_CAPTURE -ErrorAction SilentlyContinue
        Remove-Item Env:CLAUDE_ACC_TEST_EXIT_CODE -ErrorAction SilentlyContinue
        foreach ($name in $denylist) {
            [Environment]::SetEnvironmentVariable($name, $savedEnvironment[$name], 'Process')
        }
    }
    if (Test-Path -LiteralPath $managerLink) { Remove-Item -LiteralPath $managerLink -Force }
    if (Test-Path -LiteralPath $root) { Remove-Item -LiteralPath $root -Recurse -Force }
}

exit 0
