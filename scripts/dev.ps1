# 本地开发循环：跑全部检查，把输出写到仓库外的 logs\，供 AI 读取。
# 用法（在仓库根目录）：powershell -ExecutionPolicy Bypass -File scripts\dev.ps1
# 只跑某一步：scripts\dev.ps1 -Only test

param([string]$Only = "")

$root = Split-Path -Parent $PSScriptRoot
$logs = Join-Path (Split-Path -Parent $root) "logs"
New-Item -ItemType Directory -Force -Path $logs | Out-Null
Set-Location $root

# 用 cmd 重定向：保留 cargo 原始的 UTF-8 字节，避免 PowerShell 5.1 转成 UTF-16
$steps = @(
    @{ name = "1_fmt";    cmd = "cargo fmt --all --check" },
    @{ name = "2_clippy"; cmd = "cargo clippy --workspace --all-targets -- -D warnings" },
    @{ name = "3_test";   cmd = "cargo test --workspace" },
    @{ name = "4_golden"; cmd = "cargo run -q -p clip -- golden run" }
)

$summary = @()
foreach ($s in $steps) {
    if ($Only -and -not $s.name.EndsWith($Only)) { continue }
    $log = Join-Path $logs ($s.name + ".txt")
    cmd /c "$($s.cmd) > `"$log`" 2>&1"
    $code = $LASTEXITCODE
    $mark = if ($code -eq 0) { "PASS" } else { "FAIL" }
    $summary += "$mark  $($s.name)  exit=$code"
    Write-Host "$mark  $($s.name)"
}

$commit = (git rev-parse --short HEAD 2>$null)
$header = "commit=$commit  time=$(Get-Date -Format s)"
($header, $summary) | Out-File -Encoding utf8 (Join-Path $logs "0_summary.txt")
Write-Host "`n日志在 $logs"
