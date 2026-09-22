param(
  [ValidateSet('debug','release')]
  [string]$Configuration = 'release'
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$profile = if ($Configuration -eq 'release') { '--release' } else { '' }
if ($profile) { cargo build --workspace --release } else { cargo build --workspace }
$suffix = if ($Configuration -eq 'release') { 'release' } else { 'debug' }
$source = Join-Path $root ("target/{0}/roc_desk_workspace_standalone.exe" -f $suffix)
if (-not (Test-Path $source)) { throw "Build did not produce $source" }
$bin = Join-Path $root 'bin'
New-Item -ItemType Directory -Force $bin | Out-Null
$destination = Join-Path $bin 'roc_desk-workspace.exe'
Copy-Item $source $destination -Force
Write-Output "Built $destination"
