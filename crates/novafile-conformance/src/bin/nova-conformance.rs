//! `cargo run -p novafile-conformance -- generate [DIR]`: write expectation files.
fn main() {
    let mut args = std::env::args().skip(1);
    let cmd = args.next().unwrap_or_default();
    if cmd != "generate" {
        eprintln!("usage: nova-conformance generate [CONFORMANCE_DIR]");
        std::process::exit(2);
    }
    let dir = args
        .next()
        .map(std::path::PathBuf::from)
        .or_else(novafile_conformance::locate)
        .expect("no conformance directory; pass one or set NOVA_CONFORMANCE");
    let exp = novafile_conformance::expectations(&dir);
    for (path, j) in &exp {
        std::fs::write(path, novafile_conformance::render(j)).unwrap();
    }
    eprintln!("wrote {} expectation files under {}", exp.len(), dir.display());
}
