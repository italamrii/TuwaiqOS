# Comprehensive Phase 6 storage/application/recovery acceptance suite.

[CmdletBinding()]
param(
    [string]$Image,
    [string]$OutDir,
    [int]$MonitorPort = 45961,
    [int]$SerialPort = 45962,
    [switch]$AllowDirty
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$ProjectRoot = Split-Path -Parent $PSScriptRoot
Set-Location $ProjectRoot
if (-not $Image) { $Image = Join-Path $ProjectRoot "target\debug\boot-bios-tuwaiqos.img" }
$Image = [System.IO.Path]::GetFullPath($Image)
if (-not (Test-Path -LiteralPath $Image -PathType Leaf)) {
    throw "Disk image not found at '$Image'. Run scripts\build.ps1 first."
}

$Dirty = @(& git status --porcelain=v1 --untracked-files=all)
if ($Dirty.Count -gt 0 -and -not $AllowDirty) {
    throw "Worktree is dirty; commit the candidate or pass -AllowDirty for development."
}
$Commit = (& git rev-parse HEAD).Trim()
$Timestamp = Get-Date -Format "yyyyMMdd-HHmmss"
if (-not $OutDir) {
    $OutDir = Join-Path $ProjectRoot "target\phase6-storage-smoke\$($Commit.Substring(0, 12))-$Timestamp"
}
$OutDir = [System.IO.Path]::GetFullPath($OutDir)
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$TestImage = Join-Path $OutDir "storage-smoke.img"
Copy-Item -LiteralPath $Image -Destination $TestImage

$QemuCommand = Get-Command qemu-system-x86_64 -ErrorAction SilentlyContinue
$Qemu = if ($QemuCommand) { $QemuCommand.Source } else { "C:\Program Files\qemu\qemu-system-x86_64.exe" }
if (-not (Test-Path -LiteralPath $Qemu -PathType Leaf)) { throw "qemu-system-x86_64 not found." }

# -WindowStyle is a Windows-only Start-Process parameter; Unix PowerShell
# rejects it outright. QEMU is already headless (-display none) -- the style
# only hides the extra console window Windows would open for the child.
$HiddenWindowStyle = if ($PSVersionTable.PSVersion.Major -lt 6 -or $IsWindows) {
    @{ WindowStyle = 'Hidden' }
} else {
    @{}
}

$SerialLog = Join-Path $OutDir "serial.log"
$ResultsJson = Join-Path $OutDir "results.json"
$ManifestJson = Join-Path $OutDir "manifest.json"
$SerialText = [System.Text.StringBuilder]::new()
$Results = [System.Collections.Generic.List[object]]::new()
$Proc = $null
$MonitorClient = $null
$SerialClient = $null
$MonitorStream = $null
$SerialStream = $null
$MonitorWriter = $null

function Pump-Serial {
    param([int]$WaitMilliseconds = 0)
    if ($WaitMilliseconds -gt 0) { Start-Sleep -Milliseconds $WaitMilliseconds }
    while ($null -ne $SerialStream -and $SerialStream.DataAvailable) {
        $buffer = New-Object byte[] 16384
        $count = $SerialStream.Read($buffer, 0, $buffer.Length)
        if ($count -le 0) { break }
        [void]$SerialText.Append([System.Text.Encoding]::ASCII.GetString($buffer, 0, $count))
    }
}

function Save-Evidence {
    Pump-Serial
    [System.IO.File]::WriteAllText($SerialLog, $SerialText.ToString(), [System.Text.Encoding]::UTF8)
    $Results | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath $ResultsJson -Encoding UTF8
    [pscustomobject]@{
        schema = 1
        git_commit = $Commit
        source_image = $Image
        source_image_sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $Image).Hash.ToLowerInvariant()
        tested_image = $TestImage
        qemu = $Qemu
        verdict = if ($Results.Count -eq 14) { "PASS" } else { "INCOMPLETE" }
    } | ConvertTo-Json -Depth 3 | Set-Content -LiteralPath $ManifestJson -Encoding UTF8
}

function Test-Health {
    $all = $SerialText.ToString()
    if ($all -match 'KERNEL PANIC|kernel page fault|EXCEPTION: DOUBLE FAULT') {
        throw "Kernel panic/fault detected."
    }
    if ($null -ne $Proc -and $Proc.HasExited) { throw "QEMU exited unexpectedly." }
}

function Connect-Tcp {
    param([int]$Port, [string]$Name)
    $deadline = (Get-Date).AddSeconds(20)
    while ((Get-Date) -lt $deadline) {
        $client = [System.Net.Sockets.TcpClient]::new()
        try { $client.Connect("127.0.0.1", $Port); return $client } catch {
            $client.Dispose()
            Start-Sleep -Milliseconds 200
        }
    }
    throw "Timed out connecting to $Name on port $Port."
}

function Start-TestVm {
    param([Parameter(Mandatory)][string]$DiskImage)
    $arguments = @(
        "-drive", "format=raw,file=$DiskImage", "-m", "128M", "-display", "none",
        "-serial", "tcp:127.0.0.1:$SerialPort,server,nowait",
        "-monitor", "tcp:127.0.0.1:$MonitorPort,server,nowait", "-no-shutdown"
    )
    $script:Proc = Start-Process -FilePath $Qemu -ArgumentList $arguments -PassThru @HiddenWindowStyle
    $script:SerialClient = Connect-Tcp $SerialPort "serial"
    $script:MonitorClient = Connect-Tcp $MonitorPort "monitor"
    $script:SerialStream = $SerialClient.GetStream()
    $script:MonitorStream = $MonitorClient.GetStream()
    $script:MonitorWriter = [System.IO.StreamWriter]::new($MonitorStream)
    $script:MonitorWriter.AutoFlush = $true
}

function Stop-TestVm {
    if ($null -ne $MonitorWriter) {
        try { $MonitorWriter.WriteLine("quit") } catch {}
    }
    Start-Sleep -Milliseconds 400
    foreach ($resource in @($MonitorWriter, $MonitorStream, $SerialStream, $MonitorClient, $SerialClient)) {
        if ($null -ne $resource) { try { $resource.Dispose() } catch {} }
    }
    if ($null -ne $Proc -and -not $Proc.HasExited) {
        if (-not $Proc.WaitForExit(3000)) {
            Stop-Process -Id $Proc.Id -Force -ErrorAction SilentlyContinue
        }
    }
    $script:MonitorWriter = $null
    $script:MonitorStream = $null
    $script:SerialStream = $null
    $script:MonitorClient = $null
    $script:SerialClient = $null
    $script:Proc = $null
}

function Get-CheckpointHeader {
    param([Parameter(Mandatory)][string]$DiskImage, [Parameter(Mandatory)][uint32]$Lba)
    $stream = [System.IO.File]::Open($DiskImage, 'Open', 'Read', 'ReadWrite')
    try {
        [void]$stream.Seek([int64]$Lba * 512, 'Begin')
        $bytes = New-Object byte[] 512
        if ($stream.Read($bytes, 0, 512) -ne 512) { throw "Short checkpoint header read." }
        $magic = [System.Text.Encoding]::ASCII.GetString($bytes, 0, 8)
        [pscustomobject]@{
            lba = $Lba
            valid_header = $magic -eq "TQCKPT`0`0" -and [BitConverter]::ToUInt32($bytes, 24) -eq 0x434F4D54
            generation = [BitConverter]::ToUInt64($bytes, 8)
            length = [BitConverter]::ToUInt32($bytes, 16)
        }
    } finally { $stream.Dispose() }
}

function Corrupt-CheckpointPayload {
    param([Parameter(Mandatory)][string]$DiskImage, [Parameter(Mandatory)][uint32]$Lba)
    $stream = [System.IO.File]::Open($DiskImage, 'Open', 'ReadWrite', 'None')
    try {
        [void]$stream.Seek(([int64]$Lba + 1) * 512, 'Begin')
        $value = $stream.ReadByte()
        if ($value -lt 0) { throw "Cannot read checkpoint payload at LBA $Lba." }
        [void]$stream.Seek(-1, 'Current')
        $stream.WriteByte($value -bxor 0x5A)
        $stream.Flush($true)
    } finally { $stream.Dispose() }
}

function Invoke-Monitor {
    param([string]$Command, [int]$SettleMilliseconds = 35)
    $MonitorWriter.WriteLine($Command)
    Start-Sleep -Milliseconds $SettleMilliseconds
    while ($MonitorStream.DataAvailable) {
        $discard = New-Object byte[] 4096
        [void]$MonitorStream.Read($discard, 0, $discard.Length)
    }
    Pump-Serial
    Test-Health
}

function Send-Text {
    param([string]$Text)
    $map = @{ ' ' = 'spc'; '-' = 'minus'; '.' = 'dot'; '/' = 'slash' }
    foreach ($character in $Text.ToCharArray()) {
        $value = [string]$character
        if ($map.ContainsKey($value)) { Invoke-Monitor "sendkey $($map[$value])" }
        elseif ($value -cmatch '^[a-z0-9]$') { Invoke-Monitor "sendkey $value" }
        else { throw "No sendkey mapping for '$value'." }
    }
}

function Wait-Regex {
    param([string]$Pattern, [int]$Offset = 0, [int]$TimeoutSeconds = 45)
    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    while ((Get-Date) -lt $deadline) {
        Pump-Serial 100
        Test-Health
        $all = $SerialText.ToString()
        if ($Offset -gt $all.Length) { $Offset = $all.Length }
        if ($all.Substring($Offset) -match $Pattern) { return $Matches[0] }
    }
    Save-Evidence
    throw "Timed out waiting for '$Pattern'."
}

function Add-Pass {
    param([string]$Name, [string]$Evidence)
    $Results.Add([pscustomobject]@{ name = $Name; status = "PASS"; evidence = $Evidence })
    Write-Host "[PASS] $Name - $Evidence" -ForegroundColor Green
}

function Invoke-ShellCommand {
    param([string]$Command, [string[]]$Expected = @(), [int]$TimeoutSeconds = 60)
    Pump-Serial
    $offset = $SerialText.Length
    Send-Text $Command
    Invoke-Monitor "sendkey ret"
    [void](Wait-Regex "shell: command complete: $([regex]::Escape($Command))(?:\r?\n|$)" $offset $TimeoutSeconds)
    Pump-Serial 100
    $segment = $SerialText.ToString().Substring($offset)
    foreach ($pattern in $Expected) {
        if ($segment -notmatch $pattern) { throw "'$Command' missed expected '$pattern'." }
    }
    Test-Health
    return $segment
}

try {
    Start-TestVm $TestImage

    [void](Wait-Regex 'task heartbeat: beat #1(?:\r?\n|$)' 0 90)
    [void](Wait-Regex 'vfs: mounted TuwaiqFS v3 at /' 0 5)
    Add-Pass "boot and VFS mount" "kernel reached scheduler with TuwaiqFS mounted"

    $segment = Invoke-ShellCommand "mounts" @('/  TuwaiqFS v3  rw', '/boot  FAT32  ro')
    [void](Invoke-ShellCommand "cat /boot/readme.txt" @('TuwaiqOS FAT32 resource volume'))
    [void](Invoke-ShellCommand "write /boot/reject.txt no" @('Filesystem error: read-only filesystem'))
    Add-Pass "mount table and independent backend" "TuwaiqFS rw root and genuine read-only FAT32 /boot"

    $segment = Invoke-ShellCommand "vfstest" @('vfs: PASS mount-table fat32-readonly', 'file-api: PASS', 'seek') 120
    Add-Pass "VFS path, handle, seek, and hostile path coverage" ([regex]::Match($segment, 'vfs: PASS[^\r\n]*').Value)

    $segment = Invoke-ShellCommand "storagetest" @('file-mutation: PASS', 'storage: PASS') 120
    Add-Pass "mutable Ring-3 storage ABI" ([regex]::Match($segment, 'file-mutation: PASS[^\r\n]*').Value)

    [void](Invoke-ShellCommand "notes set reboot-note durable-storage" @('Saved note: reboot-note'))
    [void](Invoke-ShellCommand "notes show reboot-note" @('durable-storage'))
    Add-Pass "built-in Notes save/reopen" "Notes saved and reopened data through VFS before reboot"

    [void](Invoke-ShellCommand "ls /apps" @('desktop', 'file-manager', 'terminal', 'tuwaiq-ai', 'catalog.txt'))
    [void](Invoke-ShellCommand "cat /apps/catalog.txt" @('desktop', 'file-manager', 'terminal', 'tuwaiq-ai'))
    [void](Invoke-ShellCommand "runfs /apps/tuwaiq-ai" @('assistant service started in Ring 3', 'inference unavailable', 'exit_code=0'))
    [void](Invoke-ShellCommand "installapp hello" @('/apps/hello'))
    [void](Invoke-ShellCommand "runfs /apps/hello" @('hello: Ring 3 ELF process alive', 'exit_code=0'))
    Add-Pass "filesystem-backed ELF" "shell discovered and launched a prepackaged native app; explicit test fixture also loaded through VFS"

    [void](Invoke-ShellCommand "fsexhausttest" @('fs-exhaustion: PASS') 180)
    Add-Pass "storage exhaustion atomicity" "oversized tree rejected; old data survived and reclaimed space was reused"

    Pump-Serial
    $desktopOffset = $SerialText.Length
    Send-Text "desktop"
    Invoke-Monitor "sendkey ret"
    [void](Wait-Regex 'desktop: packaged launch path=/apps/desktop pid=[0-9]+' $desktopOffset 60)
    [void](Wait-Regex 'desktop: first frame presented' $desktopOffset 60)
    Invoke-Monitor "sendkey f"
    [void](Wait-Regex 'desktop: launch request file-manager' $desktopOffset 60)
    [void](Wait-Regex 'file-manager: started from /apps/file-manager' $desktopOffset 60)
    [void](Wait-Regex 'file-manager: listed /apps' $desktopOffset 60)
    Invoke-Monitor "sendkey b"
    [void](Wait-Regex 'file-manager: listed /boot' $desktopOffset 60)
    Invoke-Monitor "sendkey r"
    [void](Wait-Regex 'file-manager: listed /boot/DOCS' $desktopOffset 60)
    $relaunchOffset = $SerialText.Length
    Invoke-Monitor "sendkey esc"
    [void](Wait-Regex 'desktop: packaged launch path=/apps/desktop pid=[0-9]+' $relaunchOffset 60)
    Invoke-Monitor "sendkey t"
    [void](Wait-Regex 'terminal: started from /apps/terminal' $relaunchOffset 60)
    Send-Text "write session.txt phase6-desktop"
    Invoke-Monitor "sendkey ret"
    [void](Wait-Regex 'terminal: saved /data/terminal/session.txt' $relaunchOffset 60)
    Send-Text "cat /data/terminal/session.txt"
    Invoke-Monitor "sendkey ret"
    [void](Wait-Regex 'terminal: read /data/terminal/session.txt' $relaunchOffset 60)
    $secondDesktop = $SerialText.Length
    Send-Text "exit"
    Invoke-Monitor "sendkey ret"
    [void](Wait-Regex 'desktop: packaged launch path=/apps/desktop pid=[0-9]+' $secondDesktop 60)
    Invoke-Monitor "sendkey f"
    [void](Wait-Regex 'file-manager: started from /apps/file-manager' $secondDesktop 60)
    $finalDesktop = $SerialText.Length
    Invoke-Monitor "sendkey esc"
    [void](Wait-Regex 'desktop: packaged launch path=/apps/desktop pid=[0-9]+' $finalDesktop 60)
    Invoke-Monitor "sendkey esc"
    [void](Wait-Regex 'shell: command complete: desktop(?:\r?\n|$)' $desktopOffset 60)
    [void](Invoke-ShellCommand "cat /data/terminal/session.txt" @('phase6-desktop'))
    Add-Pass "desktop filesystem applications" "File Manager browsed both mounts; Terminal saved/read data; both exited and relaunched from /apps"

    [void](Invoke-ShellCommand "fsinterrupttest" @('fs-recovery: PASS') 120)

    Pump-Serial
    $rebootOffset = $SerialText.Length
    Send-Text "reboot"
    Invoke-Monitor "sendkey ret"
    [void](Wait-Regex 'TuwaiqOS v0\.5 kernel_main: booting' $rebootOffset 60)
    [void](Wait-Regex 'task heartbeat: beat #1(?:\r?\n|$)' $rebootOffset 90)
    [void](Invoke-ShellCommand "cat /data/file-mutation-test/persist.txt" @('ring3-persist-v2'))
    [void](Invoke-ShellCommand "notes show reboot-note" @('durable-storage'))
    [void](Invoke-ShellCommand "cat /data/recovery/interrupted.txt" @('stable-before-power-loss'))
    Add-Pass "genuine reboot persistence" "Ring-3 file and built-in Notes data survived reboot"

    Add-Pass "interrupted-write recovery" "uncommitted checkpoint ignored after genuine reboot; prior generation preserved"

    Pump-Serial
    $postRebootDesktop = $SerialText.Length
    Send-Text "desktop"
    Invoke-Monitor "sendkey ret"
    [void](Wait-Regex 'desktop: packaged launch path=/apps/desktop pid=[0-9]+' $postRebootDesktop 60)
    Invoke-Monitor "sendkey t"
    [void](Wait-Regex 'terminal: started from /apps/terminal' $postRebootDesktop 60)
    Send-Text "cat /data/terminal/session.txt"
    Invoke-Monitor "sendkey ret"
    [void](Wait-Regex 'terminal: read /data/terminal/session.txt' $postRebootDesktop 60)
    $postRebootExit = $SerialText.Length
    Send-Text "exit"
    Invoke-Monitor "sendkey ret"
    [void](Wait-Regex 'desktop: packaged launch path=/apps/desktop pid=[0-9]+' $postRebootExit 60)
    Invoke-Monitor "sendkey esc"
    [void](Wait-Regex 'shell: command complete: desktop(?:\r?\n|$)' $postRebootDesktop 60)
    Add-Pass "post-reboot desktop relaunch" "Terminal reopened persistent data after reboot and desktop returned safely to shell"

    [void](Invoke-ShellCommand "runfs /apps/hello" @('hello: Ring 3 ELF process alive', 'exit_code=0'))
    Test-Health
    Add-Pass "post-reboot ELF and kernel health" "filesystem ELF relaunched; no panic, page fault, or double fault"

    # Commit one generation after the interrupted-write survivor so both
    # checkpoints contain the sentinel. Corrupting the newest one must then
    # recover the older complete generation without losing the sentinel.
    [void](Invoke-ShellCommand "write /data/recovery/prime.txt prime" @('Wrote to: /data/recovery/prime.txt'))
    Stop-TestVm
    $slotA = Get-CheckpointHeader $TestImage 23490
    $slotB = Get-CheckpointHeader $TestImage 24002
    if (-not $slotA.valid_header -or -not $slotB.valid_header) {
        throw "Recovery precondition failed: both checkpoint headers must be committed."
    }
    $newest = if ($slotA.generation -gt $slotB.generation) { $slotA } else { $slotB }
    $corruptNewestImage = Join-Path $OutDir "corrupt-newest.img"
    Copy-Item -LiteralPath $TestImage -Destination $corruptNewestImage
    Corrupt-CheckpointPayload $corruptNewestImage $newest.lba
    $recoveryOffset = $SerialText.Length
    Start-TestVm $corruptNewestImage
    [void](Wait-Regex 'tuwaiqfs: checkpoint [01] checksum invalid' $recoveryOffset 90)
    [void](Wait-Regex 'tuwaiqfs: recovered from checkpoint [AB] generation [0-9]+' $recoveryOffset 30)
    [void](Wait-Regex 'task heartbeat: beat #1(?:\r?\n|$)' $recoveryOffset 90)
    [void](Invoke-ShellCommand "cat /data/recovery/interrupted.txt" @('stable-before-power-loss'))
    Add-Pass "corrupt checkpoint recovery" "newest checksum failure detected; older committed generation mounted with durable sentinel intact"
    Stop-TestVm

    # When neither checkpoint is valid, root storage must remain unavailable;
    # the kernel and shell stay alive with only the independent read-only
    # recovery backend. No empty writable filesystem may be substituted.
    $unrecoverableImage = Join-Path $OutDir "corrupt-both.img"
    Copy-Item -LiteralPath $TestImage -Destination $unrecoverableImage
    Corrupt-CheckpointPayload $unrecoverableImage $slotA.lba
    Corrupt-CheckpointPayload $unrecoverableImage $slotB.lba
    $unrecoverableOffset = $SerialText.Length
    Start-TestVm $unrecoverableImage
    [void](Wait-Regex 'vfs: TuwaiqFS unavailable; entering read-only recovery mode: no valid committed TuwaiqFS checkpoint' $unrecoverableOffset 90)
    [void](Wait-Regex 'task heartbeat: beat #1(?:\r?\n|$)' $unrecoverableOffset 90)
    [void](Invoke-ShellCommand "cat /boot/readme.txt" @('TuwaiqOS FAT32 resource volume'))
    [void](Invoke-ShellCommand "write /data/forbidden.txt no" @('Filesystem error: no VFS mount for path'))
    Add-Pass "unrecoverable corruption handling" "root stayed offline and read-only FAT32 recovery access remained available; no silent format or data acceptance"
    Stop-TestVm

    Save-Evidence
    Write-Host "Phase 6 acceptance PASS: $OutDir" -ForegroundColor Green
} finally {
    try { Save-Evidence } catch {}
    Stop-TestVm
}
