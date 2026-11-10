// The message catalogs in `locales/` are embedded by the `rust_i18n::i18n!`
// macro at compile time; this makes cargo rebuild the crate when one changes
// (a proc macro's file reads are otherwise invisible to cargo).
fn main() {
    println!("cargo:rerun-if-changed=locales");
}
