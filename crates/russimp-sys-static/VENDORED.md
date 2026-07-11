# Vendored source

This crate is based on `russimp-sys-static` 1.0.1 from
<https://github.com/EYHN/russimp-sys> and is maintained as part of this
repository.

The `assimp/` directory is an unmodified Git submodule pinned to Assimp 6.0.5
at commit `392a658f9c271be965271f45e7521a1b80ea4392`. Its bindings are generated
from that version and committed in `src/bindings.rs`, so regular builds do not
require LLVM/libclang.
