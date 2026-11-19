[CmdletBinding()]
param([switch]$MigrateOnly)

$ErrorActionPreference = 'Stop'

function Get-GitaLegacyRows([string]$Path, [string]$Delimiter) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { return }
    $utf8 = [Text.UTF8Encoding]::new($false, $true)
    try { $text = $utf8.GetString([IO.File]::ReadAllBytes($Path)).TrimStart([char]0xFEFF) }
    catch [Text.DecoderFallbackException] {
        [Text.Encoding]::RegisterProvider([Text.CodePagesEncodingProvider]::Instance)
        $codepage = [Globalization.CultureInfo]::CurrentCulture.TextInfo.ANSICodePage
        $text = [IO.File]::ReadAllText($Path, [Text.Encoding]::GetEncoding($codepage))
    }
    $reader = [IO.StringReader]::new($text)
    $parser = [Microsoft.VisualBasic.FileIO.TextFieldParser]::new($reader)
    try {
        $parser.SetDelimiters($Delimiter)
        $parser.TrimWhiteSpace = $false
        while (-not $parser.EndOfData) { ,($parser.ReadFields()) }
    }
    finally {
        $parser.Dispose()
        $reader.Dispose()
    }
}

function Import-GitaLegacyRegistry {
    $base = if ($env:GITA_PROJECT_HOME) { $env:GITA_PROJECT_HOME } else { $env:XDG_CONFIG_HOME }
    $legacy = Join-Path $(if ($base) { $base } else { Join-Path $HOME '.config' }) 'gita'
    $directory = Join-Path $(if ($base) { $base } else {
        [Environment]::GetFolderPath('ApplicationData')
    }) 'gita'
    $csv = Join-Path $legacy 'repos.csv'
    $json = Join-Path $directory 'repos.json'
    if ((Test-Path -LiteralPath $json) -or -not (Test-Path -LiteralPath $csv -PathType Leaf)) { return }
    $repos = [System.Collections.Specialized.OrderedDictionary]::new([StringComparer]::Ordinal)
    $groups = [System.Collections.Specialized.OrderedDictionary]::new([StringComparer]::Ordinal)
    foreach ($row in @(Get-GitaLegacyRows $csv ',')) {
        if ($row.Count -lt 2 -or -not $row[1] -or -not [IO.Path]::IsPathFullyQualified($row[0])) {
            throw "Invalid repository row in $csv"
        }
        $repos[$row[1]] = [ordered]@{ path = $row[0]; type = [string]$row[2]; flags = @($row[3] -split '\s+' -ne '') }
    }
    foreach ($row in @(Get-GitaLegacyRows (Join-Path $legacy 'groups.csv') ':')) {
        if ($row.Count -lt 2 -or -not $row[0]) { throw "Invalid group row in $legacy" }
        $members = @($row[1] -split '\s+' | Where-Object { $repos.Contains($_) })
        if ($members.Count) {
            $groups[$row[0]] = [ordered]@{ path = $(if ($row[2]) { $row[2] } else { $null }); repos = $members }
        }
    }
    $contexts = @(Get-ChildItem -LiteralPath $legacy -Filter '*.context' -File)
    $context = if ($contexts.Count -eq 1 -and ($contexts[0].BaseName -ceq 'auto' -or $groups.Contains($contexts[0].BaseName))) {
        $contexts[0].BaseName
    } else { $null }

    $null = New-Item -ItemType Directory -Force -Path $directory
    $temp = Join-Path $directory ("migration-" + [guid]::NewGuid().ToString('N') + '.json')
    $lock = [IO.File]::Open((Join-Path $directory '.lock'), 'OpenOrCreate', 'ReadWrite', 'ReadWrite')
    try {
        $lock.Lock(0, 1)
        if (Test-Path -LiteralPath $json) { return }
        $data = [ordered]@{ version = 1; repos = $repos; groups = $groups; context = $context }
        [IO.File]::WriteAllText($temp, ($data | ConvertTo-Json -Depth 8) + [Environment]::NewLine)
        [IO.File]::Move($temp, $json)
        Write-Host "Imported $($repos.Count) legacy gita repositories."
    }
    finally {
        $lock.Dispose()
        if (Test-Path -LiteralPath $temp) { Remove-Item -LiteralPath $temp }
    }
}

if ($MigrateOnly) {
    Import-GitaLegacyRegistry
    return
}

$architecture = switch ([Runtime.InteropServices.RuntimeInformation]::OSArchitecture) {
    Arm64 { 'arm64' }
    X64 { 'x64' }
    default { throw "Unsupported Windows architecture: $_" }
}
$repository = 'ntaksh42/gita'
$release = Invoke-RestMethod -Uri "https://api.github.com/repos/$repository/releases/latest" -Headers @{
    'User-Agent' = 'gita-installer'
}
$version = $release.tag_name -replace '^v', ''
if ($version -notmatch '^\d+\.\d+\.\d+$') { throw "Unexpected gita release tag: $($release.tag_name)" }
$name = "gita-$version-windows-$architecture.msi"
$asset = $release.assets | Where-Object name -CEQ $name | Select-Object -First 1
if (-not $asset -or $asset.digest -notmatch '^sha256:([a-fA-F0-9]{64})$') {
    throw "$($release.tag_name) has no $name with a SHA-256 digest."
}
$hash = $Matches[1]
$package = Join-Path ([IO.Path]::GetTempPath()) ("gita-$([guid]::NewGuid().ToString('N')).msi")
try {
    Invoke-WebRequest -Uri $asset.browser_download_url -OutFile $package
    if ((Get-FileHash -LiteralPath $package -Algorithm SHA256).Hash -ne $hash) {
        throw "$name failed SHA-256 verification."
    }
    Import-GitaLegacyRegistry
    $process = Start-Process -FilePath "$env:WINDIR\System32\msiexec.exe" -ArgumentList "/i ""$package"" /qn /norestart" -WindowStyle Hidden -Wait -PassThru
    if ($process.ExitCode -ne 0) { throw "gita installation failed with exit code $($process.ExitCode)" }

    $directory = Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'Programs\gita'
    $installed = Join-Path $directory 'gita.exe'
    $reported = & $installed --version
    if ($LASTEXITCODE -ne 0 -or $reported -cne "gita $version") {
        throw "Expected gita $version at $installed."
    }
    if (-not (($env:PATH -split ';') -contains $directory)) { $env:PATH = "$directory;$env:PATH" }
    Write-Host "[OK] gita installed: $version ($architecture)"
}
finally {
    if (Test-Path -LiteralPath $package) { Remove-Item -LiteralPath $package }
}
