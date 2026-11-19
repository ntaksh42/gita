$ErrorActionPreference = 'Stop'
$previousHome = $env:GITA_PROJECT_HOME
$env:GITA_PROJECT_HOME = Join-Path ([IO.Path]::GetTempPath()) ("gita-migrate-test-" + [guid]::NewGuid().ToString('N'))
$dir = Join-Path $env:GITA_PROJECT_HOME 'gita'
$null = New-Item -ItemType Directory -Path $dir
try {
    $repo = Join-Path $env:GITA_PROJECT_HOME 'directory, with spaces'
    $csv = Join-Path $dir 'repos.csv'
    $json = Join-Path $dir 'repos.json'
    $legacy = '"' + $repo + '",example,legacy,-c core.filemode=false'
    Set-Content -LiteralPath $csv -Value $legacy -Encoding utf8
    Set-Content -LiteralPath (Join-Path $dir 'groups.csv') -Value "example-group:example missing:""$repo""" -Encoding utf8
    $null = New-Item -ItemType File -Path (Join-Path $dir 'auto.context')
    & (Join-Path $PSScriptRoot 'install.ps1') -MigrateOnly
    $state = Get-Content -LiteralPath $json -Raw | ConvertFrom-Json -AsHashtable
    if ($state.version -ne 1 -or $state.context -ne 'auto' -or $state.repos.example.path -cne $repo -or
        $state.repos.example.type -cne 'legacy' -or ($state.repos.example.flags -join ' ') -cne '-c core.filemode=false' -or
        ($state.groups['example-group'].repos -join ' ') -cne 'example' -or $state.groups['example-group'].path -cne $repo) {
        throw "Migrated state did not match $csv"
    }
    if ((Get-Content -LiteralPath $csv) -cne $legacy) { throw 'Legacy CSV was modified' }
    $before = (Get-FileHash -LiteralPath $json).Hash
    Set-Content -LiteralPath $csv -Value 'not a valid csv'
    & (Join-Path $PSScriptRoot 'install.ps1') -MigrateOnly
    if ((Get-FileHash -LiteralPath $json).Hash -ne $before) { throw 'Existing JSON was overwritten' }
    Write-Host 'Legacy migration and repeat installation passed.'
}
finally {
    $env:GITA_PROJECT_HOME = $previousHome
}
