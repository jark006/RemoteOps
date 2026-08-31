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
cargo zigbuild -q -r --target aarch64-unknown-linux-musl
Write-Output "Build aarch64-unknown-linux-musl Done!"
cargo zigbuild -q -r --target x86_64-unknown-linux-musl
Write-Output "Build x86_64-unknown-linux-musl Done!"
cargo zigbuild -q -r --target riscv64gc-unknown-linux-musl
Write-Output "Build riscv64gc-unknown-linux-musl Done!"
cargo +nightly build -p remote-ops-agent -q -r --target targets/mipsel-unknown-linux-musl.json -Z json-target-spec -Z build-std=std,panic_abort
Write-Output "Build mipsel-unknown-linux-musl Done!"

Write-Output "All target builds complete!"
