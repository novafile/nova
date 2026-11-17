//! `nova-lsp`: the NOVA language server over stdio.

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (conn, io) = lsp_server::Connection::stdio();
    nova_lsp::run(conn)?;
    io.join()?;
    Ok(())
}
