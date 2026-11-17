//! `cargo run -p novafile-text --example fmt -- FILE` — document-preserving reformat.
fn main() {
    let src = std::fs::read_to_string(std::env::args().nth(1).expect("usage: fmt FILE")).unwrap();
    print!("{}", novafile_text::format_preserving(&src, novafile_text::Profile::Human).unwrap());
}
