//! `nvf-lsp`: the NOVA language server over stdio.

/// Answers `--version` and `--help`, otherwise serves LSP over stdio. Other
/// arguments are ignored: clients pass flags such as `--stdio`.
fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "-V" | "--version" => {
                println!("nvf-lsp {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }

            "-h" | "--help" => {
                println!(
                    "nvf-lsp {}: the NOVA language server\n\nUsage: nvf-lsp\n\nSpeaks the Language Server Protocol over stdin/stdout; start it from an editor.\n\nOptions:\n  -h, --help     Print help\n  -V, --version  Print version",
                    env!("CARGO_PKG_VERSION")
                );
                return Ok(());
            }

            _ => {}
        }
    }

    novafile_lsp::run_stdio()
}
