//! Embeds the model when `assets/potion-base-8M/` is present (Lane B owns this file).
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=TAP_REQUIRE_MODEL");
}
