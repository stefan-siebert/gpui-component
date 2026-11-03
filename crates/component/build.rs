use std::env;

fn main() {
    // Keep the existing expansion-time directory available for custom macro
    // calls. The shared enum and legacy adapter no longer rely on this path.
    let icons_dir = env::var("DEP_GPUI_KIT_DEFAULT_ICONS_ICONS_DIR")
        .expect("gpui-kit-assets publishes its icon directory through Cargo metadata");
    println!("cargo:rustc-env=GPUI_KIT_DEFAULT_ICONS_DIR={icons_dir}");
    println!("cargo:rerun-if-changed={icons_dir}");
    println!("cargo:rerun-if-changed=build.rs");
    // `rust_i18n::i18n!("locales")` reads the translations while the crate
    // compiles, and nothing else tells Cargo they are an input: an edited
    // `ui.yml` was not picked up until something else rebuilt the crate.
    println!("cargo:rerun-if-changed=locales");
}
