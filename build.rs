//! Compiles the Slint UI (`ui/main.slint` and everything it imports) into Rust
//! source that `slint::include_modules!()` pulls in at compile time.

fn main() {
    slint_build::compile("ui/main.slint").expect("failed to compile Slint UI");
}
