[CmdletBinding()]
param(
    [ValidateSet('aarch64-pc-windows-msvc', 'x86_64-pc-windows-msvc')]
    [string]$Target = $(if ([Runtime.InteropServices.RuntimeInformation]::OSArchitecture -eq 'Arm64') {
        'aarch64-pc-windows-msvc'
    } else { 'x86_64-pc-windows-msvc' })
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$architecture = if ($Target -eq 'aarch64-pc-windows-msvc') { 'arm64' } else { 'x64' }
$platform = if ($architecture -eq 'arm64') { 'Arm64' } else { 'x64' }

function Invoke-ComMember($Object, [string]$Name, [string]$Kind, [object[]]$Arguments) {
    try {
        $Object.GetType().InvokeMember($Name, [Reflection.BindingFlags]$Kind, $null, $Object, $Arguments)
    }
    catch { throw "Windows Installer $Kind $Name failed: $_" }
}

function Import-MsiTable([string]$Name, [string]$Columns, [string]$Types, [string]$Keys, [string[]]$Rows) {
    $fileName = "$Name.idt"
    $content = @($Columns, $Types, "$Name|$Keys") + $Rows
    $lines = $content | ForEach-Object { $_.Replace('|', [char]9) }
    [IO.File]::WriteAllLines((Join-Path $staging $fileName), [string[]]$lines, [Text.Encoding]::ASCII)
    $database.Import($staging, $fileName)
}

$metadata = cargo metadata --format-version 1 --no-deps --manifest-path (Join-Path $root 'Cargo.toml') | ConvertFrom-Json
if ($LASTEXITCODE -ne 0) { throw "cargo metadata failed ($LASTEXITCODE)" }
$version = ($metadata.packages | Where-Object name -eq gita).version
if ($version -notmatch '^\d+\.\d+\.\d+$') { throw "Expected an MSI-compatible version, got $version" }

cargo build --locked --release --target $Target --manifest-path (Join-Path $root 'Cargo.toml')
if ($LASTEXITCODE -ne 0) { throw "cargo build failed ($LASTEXITCODE)" }
$exe = Get-Item -LiteralPath (Join-Path $metadata.target_directory "$Target/release/gita.exe")
$dist = Join-Path $root 'dist'
$null = New-Item -ItemType Directory -Path $dist -Force
$staging = Join-Path $dist ("msi-" + [guid]::NewGuid().ToString('N'))
$null = New-Item -ItemType Directory -Path $staging
$fileName = "gita-$version-windows-$architecture.msi"
$msi = Join-Path $staging $fileName
$upgradeCode = '{2B9B92FB-6198-47C9-8C47-625C6E4B461C}'
$component = if ($architecture -eq 'arm64') {
    '{97670A5E-4F8B-4DE8-9F48-246320BE64D3}'
} else { '{DAE8FB4A-207A-4C63-93D6-EAB58CB61AD8}' }
# A stable product code makes the same version repairable. New releases use a major upgrade.
$identity = [Security.Cryptography.SHA256]::HashData([Text.Encoding]::UTF8.GetBytes("ntaksh42/gita/$version/$architecture"))
$productCode = [guid]::new([byte[]]$identity[0..15]).ToString('B').ToUpperInvariant()
$installer = $database = $summary = $view = $record = $null

try {
    Copy-Item -LiteralPath $exe.FullName -Destination (Join-Path $staging 'GitaExe')
    $cab = Join-Path $staging 'gita.cab'
    & makecab.exe /D CompressionType=MSZIP (Join-Path $staging 'GitaExe') $cab | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "makecab failed ($LASTEXITCODE)" }

    $installer = New-Object -ComObject WindowsInstaller.Installer
    $database = $installer.OpenDatabase($msi, 3)
    Import-MsiTable Property 'Property|Value' 's72|l0' 'Property' @(
        "ProductCode|$productCode", "UpgradeCode|$upgradeCode",
        "ProductVersion|$version", 'ProductName|gita', 'ProductLanguage|1033',
        'Manufacturer|ntaksh42', 'ARPCONTACT|ntaksh42',
        'ARPURLINFOABOUT|https://github.com/ntaksh42/gita',
        'SecureCustomProperties|GITAUPGRADE;GITANEWER'
    )
    Import-MsiTable Directory 'Directory|Directory_Parent|DefaultDir' 's72|S72|l255' 'Directory' @(
        'TARGETDIR||SourceDir', 'LocalAppDataFolder|TARGETDIR|.',
        'ProgramsFolder|LocalAppDataFolder|Programs', 'INSTALLDIR|ProgramsFolder|gita'
    )
    Import-MsiTable Component 'Component|ComponentId|Directory_|Attributes|Condition|KeyPath' 's72|S38|s72|i2|S255|S72' 'Component' @(
        "GitaComponent|$component|INSTALLDIR|256||GitaExe"
    )
    Import-MsiTable Feature 'Feature|Feature_Parent|Title|Description|Display|Level|Directory_|Attributes' 's38|S38|L64|L255|I2|i2|S72|i2' 'Feature' @(
        'Main||gita|gita command-line executable|1|1|INSTALLDIR|0'
    )
    Import-MsiTable FeatureComponents 'Feature_|Component_' 's38|s72' 'Feature_|Component_' @('Main|GitaComponent')
    Import-MsiTable File 'File|Component_|FileName|FileSize|Version|Language|Attributes|Sequence' 's72|s72|l255|i4|S72|S20|I2|i2' 'File' @(
        "GitaExe|GitaComponent|gita.exe|$($exe.Length)|||16384|1"
    )
    Import-MsiTable Media 'DiskId|LastSequence|DiskPrompt|Cabinet|VolumeLabel|Source' 'i2|i4|L64|S255|S32|S72' 'DiskId' @(
        '1|1||#gita.cab||'
    )
    Import-MsiTable Environment 'Environment|Name|Value|Component_' 's72|l255|L0|s72' 'Environment' @(
        'UserPath|=-Path|[INSTALLDIR];[~]|GitaComponent'
    )
    Import-MsiTable Upgrade 'UpgradeCode|VersionMin|VersionMax|Language|Attributes|Remove|ActionProperty' 's38|S20|S20|S255|i4|S255|s72' 'UpgradeCode|VersionMin|VersionMax|Language|Attributes' @(
        "$upgradeCode||$version||512||GITAUPGRADE",
        "$upgradeCode|$version|||2||GITANEWER"
    )
    Import-MsiTable LaunchCondition 'Condition|Description' 's255|l255' 'Condition' @(
        'NOT GITANEWER|A newer version of gita is already installed.'
    )
    Import-MsiTable InstallExecuteSequence 'Action|Condition|Sequence' 's72|S255|I2' 'Action' @(
        'FindRelatedProducts||25', 'LaunchConditions||100', 'CostInitialize||800',
        'FileCost||900', 'CostFinalize||1000', 'InstallValidate||1400',
        'InstallInitialize||1500', 'RemoveExistingProducts||1550',
        'ProcessComponents||1600', 'UnpublishFeatures||1800', 'RemoveEnvironmentStrings||3300',
        'RemoveFiles||3500', 'InstallFiles||4000', 'WriteEnvironmentStrings||5200',
        'RegisterUser||6000', 'RegisterProduct||6100', 'PublishFeatures||6300',
        'PublishProduct||6400', 'InstallFinalize||6600'
    )
    Import-MsiTable AdminExecuteSequence 'Action|Condition|Sequence' 's72|S255|I2' 'Action' @(
        'CostInitialize||800', 'FileCost||900', 'CostFinalize||1000',
        'InstallValidate||1400', 'InstallInitialize||1500', 'InstallAdminPackage||3900',
        'InstallFiles||4000', 'InstallFinalize||6600'
    )

    $view = $database.OpenView('INSERT INTO _Streams (Name, Data) VALUES (?, ?)')
    $record = $installer.CreateRecord(2)
    Invoke-ComMember $record StringData SetProperty @(1, 'gita.cab')
    $record.SetStream(2, $cab)
    $view.Execute($record)
    $view.Close()

    $summary = Invoke-ComMember $database SummaryInformation GetProperty @(20)
    Invoke-ComMember $summary Property SetProperty @(2, 'gita')
    Invoke-ComMember $summary Property SetProperty @(3, 'gita Windows installer')
    Invoke-ComMember $summary Property SetProperty @(4, 'ntaksh42')
    Invoke-ComMember $summary Property SetProperty @(7, "$platform;1033")
    Invoke-ComMember $summary Property SetProperty @(9, [guid]::NewGuid().ToString('B').ToUpperInvariant())
    Invoke-ComMember $summary Property SetProperty @(14, 500)
    Invoke-ComMember $summary Property SetProperty @(15, 2)
    $summary.Persist()
    $database.Commit()
}
finally {
    foreach ($object in @($summary, $record, $view, $database, $installer)) {
        if ($null -ne $object) { $null = [Runtime.InteropServices.Marshal]::FinalReleaseComObject($object) }
    }
}

Move-Item -LiteralPath $msi -Destination (Join-Path $dist $fileName) -Force
if ((Resolve-Path -LiteralPath $staging).Path.StartsWith("$dist\", [StringComparison]::OrdinalIgnoreCase)) {
    Remove-Item -LiteralPath $staging -Recurse -Force
}
Get-Item -LiteralPath (Join-Path $dist $fileName)
