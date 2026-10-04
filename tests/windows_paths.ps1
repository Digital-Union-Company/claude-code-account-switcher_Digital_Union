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

$tempRoot = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { $env:TEMP }
$root = Join-Path $tempRoot 'claude-acc R3B windows path identity'
$profileRoot = [Environment]::GetFolderPath('UserProfile')
$managerLink = Join-Path $profileRoot '.claude-switch'
$managerTarget = Join-Path $root 'manager'
$managerBin = Join-Path $managerTarget 'bin'
$accounts = Join-Path $managerTarget 'accounts'
$personal1 = Join-Path $accounts 'personal1'
$personal2 = Join-Path $accounts 'personal2'
$logicalPersonal1 = Join-Path (Join-Path $managerLink 'accounts') 'personal1'
$logicalPersonal2 = Join-Path (Join-Path $managerLink 'accounts') 'personal2'
$linksFile = Join-Path $managerTarget 'links'
$manager = Join-Path $managerBin 'claude-acc.exe'
$shim = Join-Path $managerBin 'claude.exe'
$trustedBin = Join-Path $root 'trusted-bin'
$fakeClaude = Join-Path $trustedBin 'claude.exe'
$captures = Join-Path $root 'captures'
$fixture = Join-Path $PSScriptRoot 'fixtures\fake_claude.rs'
$savedPath = $env:PATH
$savedCapture = $env:CLAUDE_ACC_TEST_CAPTURE
$savedExit = $env:CLAUDE_ACC_TEST_EXIT_CODE

function Write-Links {
    param([string[]]$Lines)
    if ($Lines.Count -eq 0) {
        [IO.File]::WriteAllText($linksFile, '')
    } else {
        [IO.File]::WriteAllText($linksFile, (($Lines -join "`n") + "`n"))
    }
}

function Invoke-ShimCapture {
    param([string]$Name, [string]$WorkingDirectory)
    $capture = Join-Path $captures "$Name.json"
    Remove-Item -LiteralPath $capture -Force -ErrorAction SilentlyContinue
    $env:CLAUDE_ACC_TEST_CAPTURE = $capture
    $env:CLAUDE_ACC_TEST_EXIT_CODE = '0'
    Push-Location -LiteralPath $WorkingDirectory
    try {
        & $shim '--version' | Out-Null
        $code = $LASTEXITCODE
    } finally {
        Pop-Location
    }
    Assert-Equal $code 0 "$Name shim exit"
    Assert-True (Test-Path -LiteralPath $capture -PathType Leaf) "$Name did not invoke fake Claude"
    Get-Content -LiteralPath $capture -Raw | ConvertFrom-Json
}

function Invoke-ManagerCapture {
    param([string]$Name, [string]$WorkingDirectory, [string[]]$Arguments)
    $stdout = Join-Path $captures "$Name.stdout"
    $stderr = Join-Path $captures "$Name.stderr"
    Remove-Item -LiteralPath $stdout, $stderr -Force -ErrorAction SilentlyContinue
    Push-Location -LiteralPath $WorkingDirectory
    try {
        & $manager @Arguments 1> $stdout 2> $stderr
        $code = $LASTEXITCODE
    } finally {
        Pop-Location
    }
    $stdoutText = [string]::Empty
    $stderrText = [string]::Empty
    if (Test-Path -LiteralPath $stdout) {
        $captured = Get-Content -LiteralPath $stdout -Raw
        if ($null -ne $captured) { $stdoutText = [string]$captured }
    }
    if (Test-Path -LiteralPath $stderr) {
        $captured = Get-Content -LiteralPath $stderr -Raw
        if ($null -ne $captured) { $stderrText = [string]$captured }
    }
    [pscustomobject]@{
        Code = $code
        Stdout = $stdoutText
        Stderr = $stderrText
    }
}

try {
    if (Test-Path -LiteralPath $root) { Remove-Item -LiteralPath $root -Recurse -Force }
    if (Test-Path -LiteralPath $managerLink) {
        throw 'Refusing to touch an existing real ~/.claude-switch directory.'
    }
    New-Item -ItemType Directory -Force -Path $managerBin, $personal1, $personal2, $trustedBin, $captures | Out-Null
    New-Item -ItemType Junction -Path $managerLink -Target $managerTarget | Out-Null
    Copy-Item -LiteralPath $Binary -Destination $manager -Force
    Copy-Item -LiteralPath $Binary -Destination $shim -Force
    [IO.File]::WriteAllText((Join-Path $managerTarget 'config'), "default=`n")
    Write-Links @()

    & rustc $fixture '-o' $fakeClaude
    Assert-Equal $LASTEXITCODE 0 'fake Claude compilation'
    $env:PATH = "$managerBin;$trustedBin;$savedPath"

    $caseDir = Join-Path $root 'CaseRoute\Project'
    New-Item -ItemType Directory -Force -Path $caseDir | Out-Null
    Write-Links @("$($caseDir.ToUpperInvariant())=personal1")
    $case = Invoke-ShimCapture 'case' $caseDir
    Assert-Equal ([IO.Path]::GetFullPath($case.config_dir)) ([IO.Path]::GetFullPath($logicalPersonal1)) 'case-insensitive route'

    Write-Links @("$($caseDir.Replace('\', '/'))=personal1")
    $slashes = Invoke-ShimCapture 'slashes' $caseDir
    Assert-Equal ([IO.Path]::GetFullPath($slashes.config_dir)) ([IO.Path]::GetFullPath($logicalPersonal1)) 'slash-equivalent route'

    Write-Links @("$caseDir\.\child\..\=personal1")
    $dots = Invoke-ShimCapture 'dot-trailing' $caseDir
    Assert-Equal ([IO.Path]::GetFullPath($dots.config_dir)) ([IO.Path]::GetFullPath($logicalPersonal1)) 'dot/trailing route'

    $equalsDir = Join-Path $root 'project=a=b'
    New-Item -ItemType Directory -Force -Path $equalsDir | Out-Null
    $linkEquals = Invoke-ManagerCapture 'equals-link' $equalsDir @('link', 'personal1')
    Assert-Equal $linkEquals.Code 0 'equals path link'
    $equalsLines = @(Get-Content -LiteralPath $linksFile | Where-Object { $_ -ne '' })
    Assert-True ($equalsLines -contains "$equalsDir=personal1") 'equals path did not round-trip'
    $statusEquals = Invoke-ManagerCapture 'equals-status' $equalsDir @('status')
    Assert-Equal $statusEquals.Code 0 'equals path status'
    Assert-True $statusEquals.Stdout.Contains('personal1') 'status did not resolve equals path'
    $equalsShim = Invoke-ShimCapture 'equals-shim' $equalsDir
    Assert-Equal ([IO.Path]::GetFullPath($equalsShim.config_dir)) ([IO.Path]::GetFullPath($logicalPersonal1)) 'equals path shim route'
    $unlinkEquals = Invoke-ManagerCapture 'equals-unlink' $equalsDir @('unlink')
    Assert-Equal $unlinkEquals.Code 0 'equals path unlink'
    $equalsAfterUnlink = @(Get-Content -LiteralPath $linksFile | Where-Object { $_ -ne '' })
    Assert-True ($equalsAfterUnlink -notcontains "$equalsDir=personal1") 'equals path remained after unlink'
    Assert-True ($equalsAfterUnlink.Count -gt 0) 'unlink removed an unrelated mapping'

    $target = Join-Path $root 'junction-target'
    $junction = Join-Path $root 'junction-display'
    New-Item -ItemType Directory -Force -Path $target | Out-Null
    New-Item -ItemType Junction -Path $junction -Target $target | Out-Null
    Write-Links @("$junction=personal2")
    $junctionFromTarget = Invoke-ShimCapture 'junction-from-target' $target
    Assert-Equal ([IO.Path]::GetFullPath($junctionFromTarget.config_dir)) ([IO.Path]::GetFullPath($logicalPersonal2)) 'junction mapping did not match target'
    Assert-Equal ((Get-Content -LiteralPath $linksFile -Raw).TrimEnd()) "$junction=personal2" 'lookup rewrote junction display path'
    Write-Links @("$target=personal1")
    $targetFromJunction = Invoke-ShimCapture 'target-from-junction' $junction
    Assert-Equal ([IO.Path]::GetFullPath($targetFromJunction.config_dir)) ([IO.Path]::GetFullPath($logicalPersonal1)) 'target mapping did not match junction'

    $conflictDir = Join-Path $root 'ConflictRoute'
    New-Item -ItemType Directory -Force -Path $conflictDir | Out-Null
    $conflictCapture = Join-Path $captures 'conflict-shim.json'
    $env:CLAUDE_ACC_TEST_CAPTURE = $conflictCapture
    Remove-Item -LiteralPath $conflictCapture -Force -ErrorAction SilentlyContinue
    Write-Links @(
        "$($conflictDir.ToUpperInvariant())=personal1",
        "$($conflictDir.Replace('\', '/'))/=personal2"
    )

    Push-Location -LiteralPath $conflictDir
    try {
        $shimError = & $shim '--version' 2>&1 | Out-String
        $shimCode = $LASTEXITCODE
    } finally {
        Pop-Location
    }
    Assert-True ($shimCode -ne 0) 'ambiguous shim returned success'
    Assert-True (-not (Test-Path -LiteralPath $conflictCapture)) 'ambiguous shim spawned fake Claude'
    Assert-True $shimError.Contains('different accounts') 'ambiguous shim omitted conflict diagnostic'

    foreach ($command in @(
        @{ Name = 'conflict-status'; Args = @('status') },
        @{ Name = 'conflict-whoami'; Args = @('whoami') },
        @{ Name = 'conflict-statusline'; Args = @('statusline', '--install') }
    )) {
        $result = Invoke-ManagerCapture $command.Name $conflictDir $command.Args
        Assert-True ($result.Code -ne 0) "$($command.Name) returned success"
        Assert-True $result.Stderr.Contains('different accounts') "$($command.Name) omitted conflict diagnostic"
        Assert-True (-not $result.Stdout.Contains('default')) "$($command.Name) printed fallback identity"
    }
    $conflictLinks = Invoke-ManagerCapture 'conflict-links' $conflictDir @('links')
    Assert-True ($conflictLinks.Code -ne 0) 'ambiguous links returned success'
    Assert-True $conflictLinks.Stderr.Contains('different accounts') 'links omitted conflict diagnostic'
    Assert-True $conflictLinks.Stdout.Contains('conflict') 'links hid conflicting entries'
    Assert-True (-not (Test-Path -LiteralPath (Join-Path $personal1 'settings.json'))) 'statusline mutated personal1 on conflict'
    Assert-True (-not (Test-Path -LiteralPath (Join-Path $personal2 'settings.json'))) 'statusline mutated personal2 on conflict'
    Assert-True (-not (Test-Path -LiteralPath (Join-Path $accounts 'default'))) 'statusline created accounts/default'

    $activate = Invoke-ManagerCapture 'conflict-activate' $conflictDir @('activate', '--shell', 'powershell')
    Assert-True ($activate.Code -ne 0) 'ambiguous activate returned success'
    Assert-True $activate.Stderr.Contains('different accounts') 'activate omitted conflict diagnostic'
    Assert-Equal $activate.Stdout.Trim() 'Remove-Item Env:\CLAUDE_CONFIG_DIR -ErrorAction SilentlyContinue' 'activate stdout was not a safe clear action'

    $heal = Invoke-ManagerCapture 'conflict-heal' $conflictDir @('link', 'personal1')
    Assert-Equal $heal.Code 0 'link did not heal conflict'
    $healedLines = @(Get-Content -LiteralPath $linksFile | Where-Object { $_ -ne '' })
    Assert-Equal $healedLines.Count 1 'link did not deduplicate aliases'
    Assert-True $healedLines[0].EndsWith('=personal1') 'healed link selected wrong account'
    $healed = Invoke-ShimCapture 'conflict-healed-shim' $conflictDir
    Assert-Equal ([IO.Path]::GetFullPath($healed.config_dir)) ([IO.Path]::GetFullPath($logicalPersonal1)) 'healed shim route'

    Write-Links @(
        "$($conflictDir.ToUpperInvariant())=personal1",
        "$($conflictDir.Replace('\', '/'))/=personal1"
    )
    $unlinkAliases = Invoke-ManagerCapture 'unlink-aliases' $conflictDir @('unlink')
    Assert-Equal $unlinkAliases.Code 0 'unlink aliases exit'
    Assert-Equal (Get-Content -LiteralPath $linksFile -Raw) '' 'unlink did not remove all equivalent aliases'

    Write-Host 'Windows path identity acceptance tests passed.'
} finally {
    $env:PATH = $savedPath
    $env:CLAUDE_ACC_TEST_CAPTURE = $savedCapture
    $env:CLAUDE_ACC_TEST_EXIT_CODE = $savedExit
    if (Test-Path -LiteralPath $managerLink) { Remove-Item -LiteralPath $managerLink -Force }
    if (Test-Path -LiteralPath $root) { Remove-Item -LiteralPath $root -Recurse -Force }
}

exit 0
