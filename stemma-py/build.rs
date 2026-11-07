// An extension module leaves the Python C API symbols for the interpreter to
// supply at import time. macOS linkers refuse undefined symbols unless told
// otherwise (`-undefined dynamic_lookup`); on every other target this is a
// no-op. maturin passes the same flags itself; this keeps plain
// `cargo build --workspace` linking on macOS too.
fn main() {
    pyo3_build_config::add_extension_module_link_args();
}
