<#
.SYNOPSIS
    One-time preparation of this Windows workstation as the Aionix Connect
    DEVELOPMENT / STAGING server: WSL2 platform + Docker Desktop (WSL2 backend).

.DESCRIPTION
    Must run elevated (it is started with -Verb RunAs by the dev tooling, so the only
    human action is the UAC approval). Idempotent: features already enabled and packages
    already installed are left alone. Never reboots by itself; it reports whether a
    reboot is pending in .devtools\workstation\setup-status.json.

    Steps:
      1. enable the VirtualMachinePlatform and Microsoft-Windows-Subsystem-Linux features;
      2. install/update the WSL package (no Linux distribution: Docker Desktop brings its own);
      3. install Docker Desktop from winget (official Docker Inc. package, machine scope);
      4. add the interactive user to the docker-users group.

    The Docker Subscription Service Agreement is NOT accepted here: Docker Desktop shows
    it on its first start and the user accepts it there.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)] [string] $ForUser,
    [Parameter(Mandatory)] [string] $StateDir
)

$ErrorActionPreference = 'Stop'
New-Item -ItemType Directory -Force $StateDir | Out-Null
$log = Join-Path $StateDir 'setup.log'
$statusFile = Join-Path $StateDir 'setup-status.json'
Start-Transcript -Path $log -Append | Out-Null

$status = [ordered]@{ started = (Get-Date).ToString('o'); rebootRequired = $false; steps = [ordered]@{} }
function Save-Status { $status | ConvertTo-Json -Depth 4 | Set-Content -Encoding utf8 $statusFile }

try {
    $principal = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
    if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
        throw 'setup-workstation.ps1 precisa ser executado como administrador'
    }

    foreach ($feature in 'VirtualMachinePlatform', 'Microsoft-Windows-Subsystem-Linux') {
        $f = Get-WindowsOptionalFeature -Online -FeatureName $feature
        if ($f.State -eq 'Enabled') {
            $status.steps[$feature] = 'already-enabled'
        } else {
            $r = Enable-WindowsOptionalFeature -Online -FeatureName $feature -All -NoRestart
            if ($r.RestartNeeded) { $status.rebootRequired = $true }
            $status.steps[$feature] = 'enabled'
        }
        Save-Status
    }

    # WSL package (store/MSI). Works before the reboot; the kernel starts after it.
    & wsl.exe --install --no-distribution 2>&1 | Out-String | Write-Host
    & wsl.exe --update 2>&1 | Out-String | Write-Host
    $status.steps['wsl'] = "exit $LASTEXITCODE"
    Save-Status

    $dockerExe = Join-Path $env:ProgramFiles 'Docker\Docker\Docker Desktop.exe'
    if (Test-Path $dockerExe) {
        $status.steps['docker-desktop'] = 'already-installed'
    } else {
        & winget install --id Docker.DockerDesktop --exact --scope machine --silent `
            --accept-package-agreements --accept-source-agreements --disable-interactivity 2>&1 |
            Out-String | Write-Host
        if (-not (Test-Path $dockerExe)) { throw "instalação do Docker Desktop falhou (winget exit $LASTEXITCODE)" }
        $status.steps['docker-desktop'] = 'installed'
        $status.rebootRequired = $true
    }
    Save-Status

    if (-not (Get-LocalGroup -Name 'docker-users' -ErrorAction SilentlyContinue)) {
        New-LocalGroup -Name 'docker-users' -Description 'Docker Desktop users' | Out-Null
    }
    $member = Get-LocalGroupMember -Group 'docker-users' -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -like "*\$ForUser" }
    if (-not $member) {
        Add-LocalGroupMember -Group 'docker-users' -Member $ForUser
        $status.steps['docker-users'] = "added $ForUser"
        $status.rebootRequired = $true
    } else { $status.steps['docker-users'] = 'already-member' }

    $status.finished = (Get-Date).ToString('o')
    $status.ok = $true
} catch {
    $status.ok = $false
    $status.error = $_.Exception.Message
    Write-Host "ERRO: $($_.Exception.Message)" -ForegroundColor Red
} finally {
    Save-Status
    Stop-Transcript | Out-Null
}
