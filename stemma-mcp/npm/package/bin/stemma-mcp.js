#!/usr/bin/env node
// Launcher for the stemma-mcp native binary. The real server is a Rust
// executable shipped in per-platform packages (optionalDependencies of this
// one); npm installs exactly the package whose os/cpu match the host. This
// script resolves that binary and execs it with stdio inherited, which is the
// whole job — the MCP protocol runs over the child's stdin/stdout.
"use strict";

const { spawn } = require("node:child_process");

// Keep in sync with PLATFORM_PACKAGES in ../../build-npm-packages.sh — the
// packages published from each release's binary matrix.
const PLATFORM_PACKAGES = {
  "linux x64": "@stemma-sh/mcp-linux-x64",
  "linux arm64": "@stemma-sh/mcp-linux-arm64",
  "darwin x64": "@stemma-sh/mcp-darwin-x64",
  "darwin arm64": "@stemma-sh/mcp-darwin-arm64",
  "win32 x64": "@stemma-sh/mcp-win32-x64",
};

const key = `${process.platform} ${process.arch}`;
const pkg = PLATFORM_PACKAGES[key];
if (!pkg) {
  console.error(
    `stemma-mcp: no prebuilt binary for ${key}.\n` +
      `Supported platforms: ${Object.keys(PLATFORM_PACKAGES).join(", ")}.\n` +
      `You can build from source instead (Rust >= 1.91):\n` +
      `  cargo build -p stemma-mcp --release\n` +
      `and point your MCP client at target/release/stemma-mcp.`,
  );
  process.exit(1);
}

const binName = process.platform === "win32" ? "stemma-mcp.exe" : "stemma-mcp";
let bin;
try {
  bin = require.resolve(`${pkg}/bin/${binName}`);
} catch {
  console.error(
    `stemma-mcp: platform package ${pkg} is not installed.\n` +
      `It ships as an optionalDependency; installs run with ` +
      `--omit=optional / --no-optional skip it.\n` +
      `Reinstall with optional dependencies enabled, e.g.:\n` +
      `  npm install ${pkg}\n` +
      `or run the server via npx: npx -y @stemma-sh/mcp`,
  );
  process.exit(1);
}

const child = spawn(bin, process.argv.slice(2), { stdio: "inherit" });
for (const sig of ["SIGINT", "SIGTERM"]) {
  process.on(sig, () => child.kill(sig));
}
child.on("error", (err) => {
  console.error(`stemma-mcp: failed to start ${bin}: ${err.message}`);
  process.exit(1);
});
child.on("exit", (code, signal) => {
  if (signal !== null) {
    // Re-raise so the parent's exit status reflects the child's signal death.
    process.kill(process.pid, signal);
  } else {
    process.exit(code === null ? 1 : code);
  }
});
