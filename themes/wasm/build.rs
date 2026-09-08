#[path = "../../build_support/version.rs"]
mod version;

fn main() {
    version::embed();
}
