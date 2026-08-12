use nymph_lsp::NymphLanguageServer;
use smol::Unblock;
use tower_lsp::{Client, LspService, Server};

fn main() {
	smol::block_on(async {
		// Set up logging to stderr
		tracing_subscriber::fmt()
			.with_max_level(tracing::Level::DEBUG)
			.with_writer(std::io::stderr)
			.init();

		eprintln!("Nymph Language Server starting...");

		let stdin = Unblock::new(std::io::stdin());
		let stdout = Unblock::new(std::io::stdout());

		let (service, socket) = LspService::new(|client: Client| NymphLanguageServer::new(client));
		let server = Server::new(stdin, stdout, socket);

		server.serve(service).await;
	});
}
