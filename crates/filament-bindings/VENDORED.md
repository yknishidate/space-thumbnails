# Vendored source

This crate is based on `filament-bindings` 0.2.2 from
<https://github.com/EYHN/rust-filament> and is maintained as part of this
repository.

Local changes include safer Assimp error reporting, empty-geometry checks, and
an independently maintained Assimp dependency.

Filament itself is kept unmodified as a Git submodule at `filament/`, pinned
to upstream tag `v1.73.0` (`d006c55032a8ff0f0efc8321b76016684e34c5e8`).
Compatibility changes are confined to this crate's C++ bridge and Rust
wrappers.
