import * as fs from "fs";
import * as path from "path";

const targets: Record<string, { target: string; relativePath: string }> = {
	"linux-x64": { target: "linux-x64", relativePath: path.join("server", "nymph-lsp") },
	"linux-arm64": { target: "linux-arm64", relativePath: path.join("server", "nymph-lsp") },
	"win32-x64": { target: "win32-x64", relativePath: path.join("server", "nymph-lsp.exe") },
	"win32-arm64": { target: "win32-arm64", relativePath: path.join("server", "nymph-lsp.exe") },
	"darwin-x64": { target: "darwin-x64", relativePath: path.join("server", "nymph-lsp") },
	"darwin-arm64": { target: "darwin-arm64", relativePath: path.join("server", "nymph-lsp") },
};

export function serverPayload(platform: string, arch: string) {
	const host = `${platform}-${arch}`;
	const payload = targets[host];
	if (!payload)
		throw new Error(
			`Unsupported Nymph LSP host ${host}. Install one of the six target-specific Nymph VSIX packages.`,
		);
	return payload;
}

export function resolveServerPath(options: {
	platform: string;
	arch: string;
	override?: string;
	asAbsolutePath(relativePath: string): string;
}) {
	if (options.override) {
		validateServer(options.override, options.platform, "development override");
		return options.override;
	}
	const payload = serverPayload(options.platform, options.arch);
	const server = options.asAbsolutePath(payload.relativePath);
	validateServer(server, options.platform, `payload for ${payload.target}`);
	return server;
}

function validateServer(server: string, platform: string, description: string) {
	if (!fs.existsSync(server)) {
		throw new Error(
			`Nymph LSP ${description} is missing at ${server}. Reinstall the matching target-specific VSIX.`,
		);
	}
	if (platform !== "win32") {
		try {
			fs.accessSync(server, fs.constants.X_OK);
		} catch {
			throw new Error(
				`Nymph LSP payload at ${server} is not executable. Reinstall the extension or run chmod +x on a development override.`,
			);
		}
	}
}
