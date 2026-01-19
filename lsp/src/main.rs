use nymph_lsp::NymphLanguageServer;
use tokio::io::{stdin, stdout};
use tower_lsp::{Client, LspService, Server};

#[tokio::main]
async fn main() {
	// Set up logging to stderr
	tracing_subscriber::fmt()
		.with_max_level(tracing::Level::DEBUG)
		.with_writer(std::io::stderr)
		.init();

	eprintln!("Nymph Language Server starting...");

	let stdin = stdin();
	let stdout = stdout();

	let (service, socket) = LspService::new(|client: Client| NymphLanguageServer::new(client));
	let server = Server::new(stdin, stdout, socket);

	server.serve(service).await;
}
