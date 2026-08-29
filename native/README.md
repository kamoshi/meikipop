# MeikiPop native experiments

This isolated crate is a test bed for gradually moving self-contained MeikiPop
code to Rust through [PyO3](https://pyo3.rs/) and
[Maturin](https://www.maturin.rs/). It does not change the existing Python
package or application behavior.

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

The first migrated operation is the Wayland screenshot crop. It accepts a
tightly packed BGRA frame and a `(left, top, width, height)` rectangle:

```python
cropped, width, height = meikipop_native.crop_bgra(
    frame,
    full_width,
    full_height,
    (left, top, width, height),
)
```

`cropped` is a `bytearray`, matching the buffer type expected by `mss`.

Useful Rust checks:

```bash
cargo fmt --manifest-path native/Cargo.toml --check
cargo clippy --manifest-path native/Cargo.toml --all-targets -- -D warnings
```
