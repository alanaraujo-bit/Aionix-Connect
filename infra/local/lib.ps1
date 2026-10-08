# Helpers shared by the local (Windows) development scripts. Dot-source it.

# Starts a long-running process hidden and fully detached from the caller.
#
# Start-Process creates children that inherit the caller's handles, including a
# redirected stdout pipe; whoever reads that pipe (a terminal tool, the Dev Launcher,
# a `script *> file` wrapper) then never sees EOF and waits forever. Processes created
# through WMI inherit nothing. Output goes to <Logs>\<Name>.log via cmd redirection and
# the pid of that cmd wrapper is kept in <State>\<Name>.pid (Stop-Detached kills the tree).
function Start-Detached {
    param(
        [Parameter(Mandatory)] [string] $Name,
        [Parameter(Mandatory)] [string] $Exe,
        [string[]] $Arguments = @(),
        [hashtable] $Env = @{},
        [Parameter(Mandatory)] [string] $State,
        [Parameter(Mandatory)] [string] $Logs
    )
    New-Item -ItemType Directory -Force $State, $Logs | Out-Null
    $log = Join-Path $Logs "$Name.log"
    $sets = foreach ($k in $Env.Keys) {
        $v = [string]$Env[$k]
        if ($v -match '["&|<>^%]') { throw "valor de $k contém caracteres não suportados" }
        "set `"$k=$v`"&& "
    }
    $argText = ($Arguments | ForEach-Object { if ($_ -match '\s' -and $_ -notmatch '^".*"$') { "`"$_`"" } else { $_ } }) -join ' '
    $cmdLine = "cmd.exe /d /s /c `"$($sets -join '')`"$Exe`" $argText 1>>`"$log`" 2>&1`""
    $startup = New-CimInstance -ClassName Win32_ProcessStartup -ClientOnly -Property @{ ShowWindow = [uint16]0 }
    $r = Invoke-CimMethod -ClassName Win32_Process -MethodName Create -Arguments @{
        CommandLine = $cmdLine; CurrentDirectory = (Split-Path $Exe); ProcessStartupInformation = $startup
    }
    if ($r.ReturnValue -ne 0) { throw "não foi possível iniciar $Name (WMI $($r.ReturnValue))" }
    Set-Content -Path (Join-Path $State "$Name.pid") -Value $r.ProcessId
    $r.ProcessId
}

function Stop-Detached {
    param([Parameter(Mandatory)] [string] $Name, [Parameter(Mandatory)] [string] $State)
    $pidFile = Join-Path $State "$Name.pid"
    if (Test-Path $pidFile) {
        $id = [int](Get-Content $pidFile)
        & taskkill.exe /T /F /PID $id *> $null
        Remove-Item $pidFile -Force
    }
}

function Test-LocalPort([int] $Port) {
    $c = [Net.Sockets.TcpClient]::new()
    try { $c.ConnectAsync('127.0.0.1', $Port).Wait(300) -and $c.Connected } catch { $false } finally { $c.Dispose() }
}
