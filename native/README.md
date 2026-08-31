# MeikiPop native extension

This crate contains MeikiPop components that are gradually moving to Rust
through [PyO3](https://pyo3.rs/) and [Maturin](https://www.maturin.rs/).

Build a wheel from the repository root:

```bash
maturin build --manifest-path native/Cargo.toml
```

For the fastest edit/build/import loop, use Maturin's recommended development
workflow in a writable virtual environment:

```bash
uv venv --python "$(type -P python)" --system-site-packages .venv
source .venv/bin/activate
maturin develop --manifest-path native/Cargo.toml
python -c 'import meikipop_native; print(meikipop_native.backend_name())'
```

On Wayland, MeikiPop uses the native Rust ScreenCast backend:

```bash
meikipop
```

The conservative Rust backend currently preserves the existing behavior:

- one monitor source selected through the XDG ScreenCast portal;
- one PipeWire stream consumed through GStreamer;
- the most recent frame retained as tightly packed BGRA/BGRx;
- the existing MSS-shaped Python adapter and crop coordinates.

Portal stream position and logical-size metadata are logged, but are not
applied yet. This leaves room for a later, separately tested multi-monitor and
fractional-scaling implementation without changing this first parity pass.

Useful Rust checks:

```bash
cargo fmt --manifest-path native/Cargo.toml --check
cargo test --manifest-path native/Cargo.toml --locked
cargo clippy --manifest-path native/Cargo.toml --all-targets --locked -- -D warnings
```
