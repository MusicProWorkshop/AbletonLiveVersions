# Writes the license notices of every crate compiled into LiveVersions.exe
# (normal dependencies only: build tools and proc-macros that don't end up in
# the exe are left out). Identical license texts are printed once, with the
# crates that use them.
#
#   pwsh tools/third-party-notices.ps1 [-Out THIRD-PARTY-NOTICES.txt]
#
# Run after a build (or `cargo fetch`) so the crate sources are on disk.
param(
    [string]$Out = "THIRD-PARTY-NOTICES.txt",
    [string]$Target = "x86_64-pc-windows-msvc"
)
$ErrorActionPreference = "Stop"

$meta = cargo metadata --format-version 1 --locked --filter-platform $Target | ConvertFrom-Json
if ($LASTEXITCODE -ne 0) { throw "cargo metadata failed" }

$pkgs = @{}
foreach ($p in $meta.packages) { $pkgs[$p.id] = $p }
$nodes = @{}
foreach ($n in $meta.resolve.nodes) { $nodes[$n.id] = $n }

# Walk normal (non-build, non-dev) dependency edges from the root package.
$root = $meta.resolve.root
$seen = @{}
$queue = [System.Collections.Generic.Queue[string]]::new()
$queue.Enqueue($root)
while ($queue.Count -gt 0) {
    $id = $queue.Dequeue()
    if ($seen.ContainsKey($id)) { continue }
    # Proc-macros (and what they use) run at compile time only.
    if ($pkgs[$id].targets | Where-Object { $_.kind -contains "proc-macro" }) { continue }
    $seen[$id] = $true
    foreach ($d in $nodes[$id].deps) {
        $normal = @($d.dep_kinds | Where-Object { $null -eq $_.kind }).Count -gt 0
        if ($normal) { $queue.Enqueue($d.pkg) }
    }
}
$used = $seen.Keys | Where-Object { $_ -ne $root } | ForEach-Object { $pkgs[$_] } | Sort-Object name, version

$namePattern = '^(?i)(licen[cs]e|copying|copyright|notice|unlicense|ofl|ufl)([-._ ].*)?$'
function Get-LicenseFiles($p) {
    $dir = Split-Path $p.manifest_path
    $files = @(Get-ChildItem $dir -File | Where-Object { $_.Name -match $namePattern })
    # Fonts and other bundled assets keep their licences in subfolders.
    # Subfolders: LICENSES/<id>.txt (REUSE layout), and fonts with their licence
    # .txt next to them (egui's default fonts).
    $files += @(Get-ChildItem $dir -Directory | Where-Object { $_.Name -notin @("src", "tests", "benches", "examples", "target") } |
        ForEach-Object { Get-ChildItem $_.FullName -File -Recurse -Depth 1 } | Where-Object {
            $_.Name -match $namePattern -or $_.Name -match '(?i)licen[cs]e' -or $_.Directory.Name -match '^(?i)licen[cs]es$' -or
            ($_.Extension -eq ".txt" -and (Get-ChildItem $_.DirectoryName -File | Where-Object { $_.Extension -in ".ttf", ".otf" }))
        })
    if ($p.license_file) { $files += Get-Item (Join-Path $dir $p.license_file) -ErrorAction SilentlyContinue }
    $files | Where-Object { $_ } | Sort-Object FullName -Unique
}
function Get-RepoKey($p) { if ($p.repository) { $p.repository.TrimEnd("/").ToLower() -replace '\.git$', '' -replace '/tree/.*$', '' } }

$texts = [ordered]@{}   # text -> list of "name version"
$missing = @()
foreach ($p in $used) {
    $files = @(Get-LicenseFiles $p)
    $label = "$($p.name) $($p.version)"
    if (-not $files) {
        # Crates published from one repository (egui's) often ship the licence
        # files in only some of them: use those, with the same declared licence.
        $key = Get-RepoKey $p
        $sibling = if ($key) {
            $used | Where-Object { $_.name -ne $p.name -and (Get-RepoKey $_) -eq $key -and $_.license -eq $p.license -and @(Get-LicenseFiles $_).Count } |
                Select-Object -First 1
        }
        if ($sibling) { $files = @(Get-LicenseFiles $sibling) } else { $missing += $p; continue }
    }
    foreach ($f in $files) {
        $t = ((Get-Content $f.FullName -Raw) -replace "`r`n", "`n").Trim()
        if (-not $t) { continue }
        if (-not $texts.Contains($t)) { $texts[$t] = [System.Collections.Generic.List[string]]::new() }
        if (-not $texts[$t].Contains($label)) { $texts[$t].Add($label) }
    }
}

$sb = [System.Text.StringBuilder]::new()
[void]$sb.AppendLine("Live Versions includes the following third-party software.")
[void]$sb.AppendLine("Live Versions itself is licensed under the GNU General Public License v3.0 or later (see LICENSE).")
[void]$sb.AppendLine("")
[void]$sb.AppendLine("Crates ($(@($used).Count)):")
foreach ($p in $used) {
    $src = if ($p.repository) { "  $($p.repository)" } else { "" }
    [void]$sb.AppendLine("  $($p.name) $($p.version)  [$($p.license)]$src")
}
foreach ($kv in $texts.GetEnumerator()) {
    [void]$sb.AppendLine("")
    [void]$sb.AppendLine("=" * 78)
    [void]$sb.AppendLine("Used by: " + ($kv.Value -join ", "))
    [void]$sb.AppendLine("-" * 78)
    [void]$sb.AppendLine($kv.Key)
}
$mit = @($missing | Where-Object { $_.license -match '\bMIT\b' })
$other = @($missing | Where-Object { $_.license -notmatch '\bMIT\b' })
if ($mit) {
    # No licence file in the package: the MIT terms they declare, with their authors.
    [void]$sb.AppendLine("")
    [void]$sb.AppendLine("=" * 78)
    [void]$sb.AppendLine("Used by (no license file in the package; MIT, one of the licenses they declare):")
    foreach ($p in $mit) {
        $who = if ($p.authors) { ($p.authors -replace '\s*<[^>]*>', '') -join ", " } else { "the $($p.name) developers" }
        [void]$sb.AppendLine("  $($p.name) $($p.version): Copyright (c) $who")
    }
    [void]$sb.AppendLine("-" * 78)
    [void]$sb.AppendLine(@"
Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
"@.Replace("`r`n", "`n").Trim())
}
if ($other) {
    [void]$sb.AppendLine("")
    [void]$sb.AppendLine("=" * 78)
    [void]$sb.AppendLine("These crates ship no license file; their declared license applies:")
    foreach ($p in $other) { [void]$sb.AppendLine("  $($p.name) $($p.version) ($($p.license))") }
}
[System.IO.File]::WriteAllText($ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Out),$sb.ToString().Replace("`r`n", "`n").Replace("`n", "`r`n"))
"$Out`: $(@($used).Count) crates, $($texts.Count) distinct license texts, $($missing.Count) without a license file"
if ($missing) { "  without a license file: " + (($missing | ForEach-Object { "$($_.name) $($_.version) ($($_.license))" }) -join "; ") }
