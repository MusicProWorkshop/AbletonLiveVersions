# Builds LiveVersions.exe.
#   .\build.ps1             debug build   -> target\debug\LiveVersions.exe
#   .\build.ps1 --release   release build -> target\release\LiveVersions.exe
#
# Needs Rust (MSVC toolchain) and a Visual Studio install with the MSVC linker and
# the Windows SDK. No C compiler is required. The script points cargo at the
# linker and libraries directly, so it also works with partial VS installs.
$ErrorActionPreference = "Stop"

function Find-Msvc {
    $roots = @("$env:ProgramFiles\Microsoft Visual Studio", "${env:ProgramFiles(x86)}\Microsoft Visual Studio")
    $tools = foreach ($r in $roots) {
        if (Test-Path $r) { Get-ChildItem $r -Directory -Recurse -Depth 4 -Filter MSVC -ErrorAction SilentlyContinue |
            Where-Object { $_.Parent.Name -eq "Tools" } | ForEach-Object { Get-ChildItem $_.FullName -Directory } }
    }
    foreach ($t in ($tools | Sort-Object Name -Descending)) {
        $link = Join-Path $t.FullName "bin\Hostx64\x64\link.exe"
        $lib = @("lib\x64", "lib\onecore\x64") | ForEach-Object { Join-Path $t.FullName $_ } |
            Where-Object { Test-Path (Join-Path $_ "msvcrt.lib") } | Select-Object -First 1
        if ((Test-Path $link) -and $lib) { return @{ Bin = Split-Path $link; Lib = $lib } }
    }
    throw "MSVC linker not found. Install Visual Studio Build Tools with 'Desktop development with C++'."
}

function Find-Sdk {
    $kits = "${env:ProgramFiles(x86)}\Windows Kits\10"
    $ver = Get-ChildItem "$kits\Lib" -Directory | Where-Object { Test-Path "$($_.FullName)\um\x64\kernel32.lib" } |
        Sort-Object Name -Descending | Select-Object -First 1
    if (-not $ver) { throw "Windows SDK not found." }
    $rc = Get-ChildItem "$kits\bin" -Recurse -Filter rc.exe -ErrorAction SilentlyContinue |
        Where-Object { $_.Directory.Name -eq "x64" } | Sort-Object FullName -Descending | Select-Object -First 1
    return @{ Um = "$($ver.FullName)\um\x64"; Ucrt = "$($ver.FullName)\ucrt\x64"; RcDir = $rc.DirectoryName }
}

$msvc = Find-Msvc
$sdk = Find-Sdk
$env:LIB = "$($msvc.Lib);$($sdk.Um);$($sdk.Ucrt)"
$env:PATH = "$($msvc.Bin);$($sdk.RcDir);$env:PATH"
$env:CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER = Join-Path $msvc.Bin "link.exe"
Set-Location $PSScriptRoot
# ".\build.ps1 test" runs the unit tests; ".\build.ps1 test-all" also the ones that
# drive a real Save dialog (they take keyboard focus for a moment).
# Anything else is passed to "cargo build".
if ($args.Count -gt 0 -and $args[0] -eq "test") {
    cargo test
} elseif ($args.Count -gt 0 -and $args[0] -eq "test-all") {
    cargo test "--" "--include-ignored" "--test-threads=1"
} else {
    cargo build @args
}
exit $LASTEXITCODE
