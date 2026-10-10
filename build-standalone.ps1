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

# 2026-10 用户需求：构建完默认再把产物拷一份进 roc_desk-releases\bundle\，不用
# 每次手动搬——这是多拷一份，不影响上面已经写好的 bin\（本地快速验证用）。
# releases 仓库是约定中的同级目录（和这个仓库一起 clone 在 roc_tools\ 下），
# 这台机器上没 clone 这个仓库就跳过，不算构建失败。
$releasesBundle = Join-Path $root '..\roc_desk-releases\bundle'
if (Test-Path -LiteralPath $releasesBundle) {
    Copy-Item -LiteralPath $destination -Destination (Join-Path $releasesBundle (Split-Path $destination -Leaf)) -Force
    Write-Output "Also copied to $releasesBundle"
} else {
    Write-Warning "roc_desk-releases\bundle not found at $releasesBundle, skipped release copy"
}
