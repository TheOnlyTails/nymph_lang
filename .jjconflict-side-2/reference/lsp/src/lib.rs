#![warn(clippy::all)]

pub mod analyzer;
pub mod document;
pub mod semantic_tokens;
pub mod server;
pub mod symbols;
pub mod workspace;

pub use server::NymphLanguageServer;
