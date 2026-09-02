cargo zigbuild -q -r --target aarch64-apple-darwin
Write-Output "Build aarch64-apple-darwin Done!"
cargo zigbuild -q -r --target x86_64-apple-darwin
Write-Output "Build x86_64-apple-darwin Done!"
cargo zigbuild -q -r --target x86_64-pc-windows-gnullvm
Write-Output "Build x86_64-pc-windows-gnullvm Done!"
cargo zigbuild -q -r --target aarch64-pc-windows-gnullvm
Write-Output "Build aarch64-pc-windows-gnullvm Done!"
cargo zigbuild -q -r --target armv7-unknown-linux-musleabihf
Write-Output "Build armv7-unknown-linux-musleabihf Done!"
cargo zigbuild -q -r --target armv7-unknown-linux-musleabi
Write-Output "Build armv7-unknown-linux-musleabi Done!"
cargo zigbuild -q -r --target aarch64-unknown-linux-musl
Write-Output "Build aarch64-unknown-linux-musl Done!"
cargo zigbuild -q -r --target x86_64-unknown-linux-musl
Write-Output "Build x86_64-unknown-linux-musl Done!"
cargo zigbuild -q -r --target riscv64gc-unknown-linux-musl
Write-Output "Build riscv64gc-unknown-linux-musl Done!"
cargo zigbuild -q -r --target loongarch64-unknown-linux-musl
Write-Output "Build loongarch64-unknown-linux-musl Done!"
cargo +nightly build -p remote-ops-agent -q -r --target targets/mipsel-unknown-linux-musl.json -Z json-target-spec -Z build-std=std,panic_abort
Write-Output "Build mipsel-unknown-linux-musl Done!"

Write-Output "All target builds complete!"

# ---- Collect release binaries into Releases/ ----
$releaseDir = Join-Path $PSScriptRoot "Releases"
if (-not (Test-Path $releaseDir)) {
    New-Item -ItemType Directory -Path $releaseDir | Out-Null
    Write-Output "Created $releaseDir"
}

# target triple -> destination suffix (Windows 产物自带 .exe)
$collectTargets = @(
    @{ triple = "aarch64-apple-darwin";          suffix = "arm64-mac" },
    @{ triple = "x86_64-apple-darwin";           suffix = "x64-mac" },
    @{ triple = "x86_64-pc-windows-gnullvm";     suffix = "x64-win.exe" },
    @{ triple = "aarch64-pc-windows-gnullvm";    suffix = "arm64-win.exe" },
    @{ triple = "armv7-unknown-linux-musleabihf";suffix = "arm32hf-linux" },
    @{ triple = "armv7-unknown-linux-musleabi";   suffix = "arm32-linux" },
    @{ triple = "aarch64-unknown-linux-musl";    suffix = "arm64-linux" },
    @{ triple = "x86_64-unknown-linux-musl";     suffix = "x64-linux" },
    @{ triple = "riscv64gc-unknown-linux-musl";  suffix = "riscv64gc-linux" },
    @{ triple = "loongarch64-unknown-linux-musl"; suffix = "loongarch64-linux" },
    @{ triple = "mipsel-unknown-linux-musl";     suffix = "mipsel-linux" }
)

foreach ($t in $collectTargets) {
    $srcDir = Join-Path $PSScriptRoot ("target/" + $t.triple + "/release")
    $exe = if ($t.suffix -like "*.exe") { ".exe" } else { "" }
    foreach ($bin in @("remote-ops-agent", "remote-ops-proxy")) {
        # mipsel 只编译 agent
        if ($t.triple -eq "mipsel-unknown-linux-musl" -and $bin -eq "remote-ops-proxy") { continue }
        $src = Join-Path $srcDir ($bin + $exe)
        if (Test-Path $src) {
            Copy-Item -Path $src -Destination (Join-Path $releaseDir ($bin + "-" + $t.suffix)) -Force
            Write-Output "Copied $bin-$($t.suffix)"
        } else {
            Write-Output "Warning: $src not found"
        }
    }
}

Write-Output "All release binaries collected into $releaseDir!"
