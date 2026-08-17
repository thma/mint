//! Locate the system libsoxr and tell rustc how to link it.
//!
//! This was `libsoxr-sys`'s build script until mint bound the C API itself (see
//! `src/ops/soxr.rs` for why). It is the same probe: pkg-config, no vendoring, no fallback.
//! `pkg_config::probe_library` emits the `cargo:rustc-link-*` directives, and because the
//! system libsoxr is a shared library that means a **dynamic** link — which is the licence
//! decision as much as the build one, since static linking would make every mint binary a
//! combined work under LGPL-2.1 §6. See THIRD-PARTY-LICENSES.md.
fn main() {
    // Probing is pointless on docs.rs (no C libraries there) and its failure would be the
    // only thing anyone saw of the crate.
    if std::env::var_os("DOCS_RS").is_some() {
        return;
    }
    if let Err(err) = pkg_config::probe_library("soxr") {
        panic!(
            "could not find libsoxr via pkg-config.\n\n\
             mint links libsoxr for sample-rate conversion; there is no pure-Rust fallback, \
             so this is a hard build failure rather than a degraded build.\n\n\
             \x20 macOS          brew install libsoxr pkg-config\n\
             \x20 Debian/Ubuntu  apt-get install libsoxr-dev pkg-config\n\
             \x20 Fedora/RHEL    dnf install soxr-devel pkgconf-pkg-config\n\
             \x20 Arch           pacman -S soxr pkgconf\n\
             \x20 Windows        vcpkg install soxr, then set PKG_CONFIG_PATH at its \
             lib/pkgconfig\n\n\
             pkg-config said:\n{err}"
        );
    }
}
