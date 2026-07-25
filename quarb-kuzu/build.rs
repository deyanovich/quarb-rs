// Exists only to host the build-dependency below: kuzu 0.11.3
// pins `cxx = "=1.0.138"` but lets cxx-build float, and cxx-build
// 1.0.198's generated C++ mangles bridge symbols differently than
// the 1.0.138 macro side expects — the final link then fails with
// undefined `kuzu_rs$cxxbridge1$…` symbols. Pinning cxx-build here
// forces a matched pair for everyone who builds this crate.
fn main() {}
