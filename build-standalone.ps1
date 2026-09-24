param(
  [ValidateSet('debug','release')]
  [string]$Configuration = 'release'
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $MyInvocation.MyCommand.Path

# 前端产物之前是直接提交进 git 的 `standalone/dist/`（2026-09 发现这个反模式并
# 改成 gitignore 之后，这一步之前缺失，导致这个脚本和 CI 都会因为 `dist/` 不存在
# 而在 `tauri::generate_context!()` 报错）——vite outDir 配置成直接写到
# `standalone/dist`（见 src-web/vite.config.ts），这里显式跑一遍前端构建，不依赖
# 任何提前存在于工作区的产物。
$srcWeb = Join-Path $root 'src-web'
Push-Location $srcWeb
try {
    npm install
    if ($LASTEXITCODE -ne 0) { throw "npm install failed with exit code $LASTEXITCODE" }
    npm run build
    if ($LASTEXITCODE -ne 0) { throw "npm run build failed with exit code $LASTEXITCODE" }
} finally {
    Pop-Location
}

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
