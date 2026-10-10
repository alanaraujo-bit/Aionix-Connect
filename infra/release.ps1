<#
.SYNOPSIS
    Publishes a new version of the desktop apps: installers, signed update channel and
    website, in one command.

.DESCRIPTION
    1. checks a clean git tree and the release key
    2. sets the workspace version (Cargo.toml) and commits "Release vX.Y.Z"
    3. builds aionix-connect with the production server and update channel compiled
       in, then aionix-setup with the app embedded
    4. copies the installers to site/public/releases/<version>/ (not committed)
    5. writes site/public/updates/app/latest.json + .sig, signed with the release
       key (outside the repo), and verifies them against the keys compiled into the apps
    6. deploys the site to production (Vercel) and re-verifies what is publicly served
    7. tags vX.Y.Z

    The private key lives in %USERPROFILE%\.aionix\release-signing.key (only this user
    and SYSTEM can read it; outside AppData so Store-installed PowerShell sees it too). Back it up: without it, installed apps can never
    receive another update.

.EXAMPLE
    .\infra\release.ps1 -Version 0.2.0 -Notes "Instalador e atualização automática"
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)] [string] $Version,
    [string[]] $Notes = @(),
    [string] $Server = 'https://connect.aionixdev.com',
    [string] $Site = 'https://aionixconnect.aionixdev.com',
    [string] $KeyFile = (Join-Path $env:USERPROFILE '.aionix\release-signing.key'),
    [switch] $SkipDeploy,
    [switch] $AllowDirty
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$Repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$Target = Join-Path $Repo 'target\release'
$SiteDir = Join-Path $Repo 'site'

function Step([string] $t) { Write-Host "`n==> $t" -ForegroundColor Cyan }
function Fail([string] $t) { Write-Host "ERRO: $t" -ForegroundColor Red; exit 1 }
function Run([string] $exe, [string[]] $argv) {
    & $exe @argv
    if ($LASTEXITCODE -ne 0) { Fail "$exe $($argv -join ' ') falhou ($LASTEXITCODE)" }
}

if ($Version -notmatch '^\d+\.\d+\.\d+$') { Fail "versão inválida: $Version (use X.Y.Z)" }
if (-not (Test-Path $KeyFile)) { Fail "chave de release não encontrada: $KeyFile" }

Push-Location $Repo
try {
    Step 'Verificações'
    if (-not $AllowDirty -and (& git status --porcelain)) { Fail 'há alterações não commitadas' }
    if (& git tag --list "v$Version") { Fail "a tag v$Version já existe" }

    Step "Versão $Version"
    $cargo = Join-Path $Repo 'Cargo.toml'
    $text = [IO.File]::ReadAllText($cargo)
    $new = [regex]::Replace($text, '(?m)(^\[workspace\.package\][\s\S]*?^version\s*=\s*")[^"]+(")', "`${1}$Version`${2}", 1)
    if ($new -ne $text) {
        [IO.File]::WriteAllText($cargo, $new)
        Run cargo @('update', '--workspace', '--offline')
        Run git @('add', 'Cargo.toml', 'Cargo.lock')
        Run git @('commit', '-q', '-m', "Release v$Version")
    }

    Step 'Build dos aplicativos (servidor e canal de atualização embutidos)'
    $env:AIONIX_DEFAULT_SERVER = $Server
    $env:AIONIX_UPDATE_BASE = "$Site/updates"
    Run cargo @('build', '--release', '-p', 'aionix-desktop', '--bin', 'aionix-connect')
    Run cargo @('build', '--release', '-p', 'aionix-update', '--bin', 'aionix-release')

    $products = @(
        @{ slug = 'app'; exe = 'aionix-connect.exe'; file = "Aionix-Connect-Setup-$Version.exe" }
    )
    $relDir = Join-Path $SiteDir "public\releases\$Version"
    Remove-Item (Join-Path $SiteDir 'public\releases') -Recurse -Force -ErrorAction SilentlyContinue
    Remove-Item (Join-Path $SiteDir 'public\updates') -Recurse -Force -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Force $relDir | Out-Null
    $release = Join-Path $Target 'aionix-release.exe'
    $noteArgs = @(); foreach ($n in $Notes) { $noteArgs += @('--note', $n) }

    foreach ($p in $products) {
        Step "Instalador: $($p.slug)"
        $env:AIONIX_SETUP_PAYLOAD = Join-Path $Target $p.exe
        Run cargo @('build', '--release', '-p', 'aionix-setup')
        $out = Join-Path $relDir $p.file
        Copy-Item (Join-Path $Target 'aionix-setup.exe') $out -Force
        Write-Host ("  {0} ({1:N1} MB)" -f $p.file, ((Get-Item $out).Length / 1MB))

        $updDir = Join-Path $SiteDir "public\updates\$($p.slug)"
        Run $release (@('manifest', '--key', $KeyFile, '--product', $p.slug, '--version', $Version,
            '--installer', $out, '--url', "$Site/releases/$Version/$($p.file)", '--out', $updDir) + $noteArgs)
        Run $release @('verify', '--product', $p.slug, '--manifest', (Join-Path $updDir 'latest.json'), '--installer', $out)
    }
    Remove-Item Env:AIONIX_SETUP_PAYLOAD, Env:AIONIX_DEFAULT_SERVER, Env:AIONIX_UPDATE_BASE -ErrorAction SilentlyContinue

    Run git @('add', 'site/public/updates')
    & git diff --cached --quiet
    if ($LASTEXITCODE -ne 0) { Run git @('commit', '-q', '-m', "Update channel v$Version") }

    if (-not $SkipDeploy) {
        Step 'Deploy do site (Vercel, produção)'
        Push-Location $SiteDir
        try { Run vercel @('deploy', '--prod', '--yes') } finally { Pop-Location }

        Step 'Conferência do que está publicado'
        $tmp = Join-Path ([IO.Path]::GetTempPath()) "aionix-release-check-$Version"
        New-Item -ItemType Directory -Force $tmp | Out-Null
        foreach ($p in $products) {
            $d = Join-Path $tmp $p.slug; New-Item -ItemType Directory -Force $d | Out-Null
            $base = "$Site/updates/$($p.slug)/latest.json"
            Invoke-WebRequest $base -OutFile (Join-Path $d 'latest.json') -UseBasicParsing
            Invoke-WebRequest "$base.sig" -OutFile (Join-Path $d 'latest.json.sig') -UseBasicParsing
            Invoke-WebRequest "$Site/releases/$Version/$($p.file)" -OutFile (Join-Path $d $p.file) -UseBasicParsing
            Run $release @('verify', '--product', $p.slug, '--manifest', (Join-Path $d 'latest.json'), '--installer', (Join-Path $d $p.file))
        }
        Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
    }

    Run git @('tag', '-a', "v$Version", '-m', "Aionix Connect $Version")
    Write-Host "`nRelease $Version publicada." -ForegroundColor Green
    Write-Host "  Site: $Site"
    Write-Host "  git push --follow-tags quando quiser enviar ao repositório remoto."
}
finally { Pop-Location }
