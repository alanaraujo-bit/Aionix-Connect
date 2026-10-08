<#
.SYNOPSIS
    Continues the workstation preparation after the reboot that activates WSL2:
    starts Docker Desktop, brings up the development databases and the staging stack,
    runs the whole test suite with integration tests required, and records the results.

.DESCRIPTION
    Registered once (HKCU RunOnce) by `-Register`; runs in a visible window at the next
    sign-in. Docker Desktop asks, on its very first start, for its terms of use to be
    accepted — that click is the user's. Everything else is automatic.

    Results: .data\dev\after-reboot.log and .data\dev\after-reboot-status.json.

.EXAMPLE
    .\infra\local\after-reboot.ps1 -Register   # schedule for the next sign-in
    .\infra\local\after-reboot.ps1             # run now
#>
[CmdletBinding()]
param([switch] $Register)

$ErrorActionPreference = 'Stop'
$Repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$Dev = Join-Path $Repo '.data\dev'
New-Item -ItemType Directory -Force $Dev | Out-Null
$StatusFile = Join-Path $Dev 'after-reboot-status.json'

if ($Register) {
    $cmd = "`"$((Get-Command pwsh).Source)`" -NoProfile -ExecutionPolicy Bypass -File `"$PSCommandPath`""
    New-ItemProperty -Path 'HKCU:\Software\Microsoft\Windows\CurrentVersion\RunOnce' -Name 'AionixConnectAfterReboot' -Value $cmd -PropertyType String -Force | Out-Null
    Write-Host "Agendado para o próximo login: $cmd"
    return
}

$Host.UI.RawUI.WindowTitle = 'Aionix Connect — preparando o ambiente'
Start-Transcript -Path (Join-Path $Dev 'after-reboot.log') -Append | Out-Null
$status = [ordered]@{ started = (Get-Date).ToString('o'); steps = [ordered]@{} }
function Save-Status { $status | ConvertTo-Json -Depth 4 | Set-Content -Encoding utf8 $StatusFile }
function Step([string] $name, [string] $value) { $status.steps[$name] = $value; Save-Status; Write-Host "[$name] $value" -ForegroundColor Cyan }

$docker = 'C:\Program Files\Docker\Docker\resources\bin\docker.exe'
try {
    Write-Host 'Aionix Connect: preparando o servidor local após a reinicialização.' -ForegroundColor Green
    Write-Host 'Se o Docker Desktop pedir, aceite os termos de uso. O resto é automático.' -ForegroundColor Yellow

    # Native development services use the same ports as the dev compose file.
    & pwsh -NoProfile -File (Join-Path $PSScriptRoot 'native-stack.ps1') -Action Down | Out-Null
    Step 'native-stack' 'stopped'

    $env:Path = "C:\Program Files\Docker\Docker\resources\bin;$env:Path"
    & $docker info --format '{{.ServerVersion}}' *> $null
    if ($LASTEXITCODE -ne 0) {
        Start-Process 'C:\Program Files\Docker\Docker\Docker Desktop.exe'
        $deadline = (Get-Date).AddMinutes(20)
        do {
            Start-Sleep -Seconds 5
            & $docker info --format '{{.ServerVersion}}' *> $null
        } until ($LASTEXITCODE -eq 0 -or (Get-Date) -gt $deadline)
        if ($LASTEXITCODE -ne 0) { throw 'o Docker Desktop não ficou pronto em 20 minutos' }
    }
    Step 'docker' ((& $docker version --format 'engine {{.Server.Version}}, client {{.Client.Version}}') -join ' ')
    Step 'compose' ((& $docker compose version --short) -join ' ')

    Push-Location $Repo
    try {
        & $docker compose up -d --wait
        if ($LASTEXITCODE -ne 0) { throw 'docker compose up (bancos de desenvolvimento) falhou' }
        Step 'dev-databases' 'PostgreSQL 17.6 on 55432, Redis 7.4 on 56379 (healthy)'

        $env:AIONIX_REQUIRE_INTEGRATION = '1'
        $testLog = Join-Path $Dev 'after-reboot-tests.log'
        & cargo test --workspace --no-fail-fast 2>&1 | Tee-Object -FilePath $testLog | Select-String -Pattern 'test result|FAILED|panicked' | ForEach-Object { Write-Host $_.Line }
        $passed = (Select-String -Path $testLog -Pattern 'test result: \w+\. (\d+) passed' -AllMatches | ForEach-Object { [int]$_.Matches[0].Groups[1].Value } | Measure-Object -Sum).Sum
        $failed = (Select-String -Path $testLog -Pattern '(\d+) failed' -AllMatches | ForEach-Object { [int]$_.Matches[0].Groups[1].Value } | Measure-Object -Sum).Sum
        Step 'tests' "passed=$passed failed=$failed (log: $testLog)"

        & pwsh -NoProfile -File (Join-Path $PSScriptRoot 'staging.ps1') -Action Up
        Step 'staging' $(if ($LASTEXITCODE -eq 0) { 'up and healthy' } else { "failed ($LASTEXITCODE)" })
    } finally { Pop-Location }
    $status.ok = $true
} catch {
    $status.ok = $false
    $status.error = $_.Exception.Message
    Write-Host "ERRO: $($_.Exception.Message)" -ForegroundColor Red
} finally {
    $status.finished = (Get-Date).ToString('o')
    Save-Status
    Stop-Transcript | Out-Null
}
Write-Host "`nConcluído. Volte ao Claude e diga 'continuar' — os resultados estão em .data\dev\after-reboot-status.json." -ForegroundColor Green
Read-Host 'Pressione Enter para fechar'
