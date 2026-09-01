# meikipop-native

Pure Rust implementation of MeikiPop's dictionary, OCR, screenshot capture,
and worker pipeline.

Build and test it with Cargo:

```bash
cargo build --manifest-path native/Cargo.toml
cargo test --manifest-path native/Cargo.toml
```

The standalone Slint application consumes this crate as a normal Rust library:

```bash
cargo run --manifest-path apps/gui-slint/Cargo.toml
```
