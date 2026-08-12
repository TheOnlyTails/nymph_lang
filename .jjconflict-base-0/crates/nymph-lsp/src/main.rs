//! Binary entry point: connects the server loop to real stdio. See
//! `nymph_lsp::run` for the protocol handshake and message loop.

use lsp_server::Connection;

fn main() -> anyhow::Result<()> {
	let (connection, io_threads) = Connection::stdio();
	nymph_lsp::run(connection)?;
	io_threads.join()?;
	Ok(())
}
