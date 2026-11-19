[CmdletBinding()]
param([Parameter(Mandatory)][string]$Package)

$ErrorActionPreference = 'Stop'
$packagePath = (Resolve-Path -LiteralPath $Package).Path
$root = Join-Path ([IO.Path]::GetTempPath()) ("gita-msi-test-" + [guid]::NewGuid().ToString('N'))
$null = New-Item -ItemType Directory -Path $root
$install = Join-Path $root 'installed'
$envKey = 'HKCU:\Environment'
$installed = $false

function Invoke-Msi([string]$Arguments) {
    $process = Start-Process -FilePath "$env:WINDIR\System32\msiexec.exe" -ArgumentList $Arguments -WindowStyle Hidden -Wait -PassThru
    if ($process.ExitCode -ne 0) {
        Get-ChildItem -LiteralPath $root -Filter '*.log' | ForEach-Object {
            Get-Content -LiteralPath $_.FullName -Tail 60 | Write-Host
        }
        throw "msiexec failed with exit code $($process.ExitCode). Logs: $root"
    }
}

function Test-InstallPath {
    $userPath = Get-ItemPropertyValue -Path $envKey -Name Path -ErrorAction Ignore
    @(($userPath -split ';') | ForEach-Object { $_.TrimEnd('\') }) -contains $install
}

# Keep this integration test separate from any existing installation of the product.
$installer = New-Object -ComObject WindowsInstaller.Installer
$related = $installer.GetType().InvokeMember('RelatedProducts', 'GetProperty', $null, $installer,
    @('{2B9B92FB-6198-47C9-8C47-625C6E4B461C}'))
$count = $related.GetType().InvokeMember('Count', 'GetProperty', $null, $related, @())
if ($count -ne 0) { throw 'Run the installer test on a clean account without a gita MSI installation.' }

try {
    Invoke-Msi "/i ""$packagePath"" /qn /norestart INSTALLDIR=""$install"" /L*v ""$root\install.log"""
    $installed = $true
    if (-not (Test-InstallPath)) { throw 'MSI did not add the installation directory to user PATH.' }
    $exe = Join-Path $install 'gita.exe'
    if ((& $exe --version) -notmatch '^gita \d+\.\d+\.\d+$' -or $LASTEXITCODE -ne 0) {
        throw 'Installed gita did not report its version.'
    }
    $before = Join-Path $root 'config.json'
    Set-Content -LiteralPath $before -Value 'keep'
    Invoke-Msi "/x ""$packagePath"" /qn /norestart /L*v ""$root\uninstall.log"""
    $installed = $false
    if ((Test-Path -LiteralPath $exe) -or (Test-InstallPath)) { throw 'MSI uninstall left gita installed.' }
    if ((Get-Content -LiteralPath $before) -ne 'keep') { throw 'MSI uninstall unexpectedly modified test data.' }
    Write-Host 'MSI installation, version, user PATH and uninstallation passed.'
}
finally {
    if ($installed) { Invoke-Msi "/x ""$packagePath"" /qn /norestart /L*v ""$root\cleanup.log""" }
}
