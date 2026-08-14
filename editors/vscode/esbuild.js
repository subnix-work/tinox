// Bundles src/extension.ts + its dependencies (vscode-languageclient and
// its transitive deps) into a single self-contained out/extension.js.
// Real bug this exists to prevent, hit live: plain `tsc` compiles
// extension.ts but leaves `require("vscode-languageclient/node")` as a
// literal Node require -- .vscodeignore's `node_modules/**` then strips
// that module out of the packaged .vsix entirely, so the require throws
// the moment VS Code loads the extension, activate() never runs, and
// there's no visible error beyond a silent fallback to VS Code's own
// generic word-based suggestions. Bundling means the packaged extension
// needs nothing from node_modules at runtime -- only `vscode` itself
// stays external, since that's injected by the VS Code runtime, not a
// real npm package.
const esbuild = require("esbuild");

const watch = process.argv.includes("--watch");

const options = {
  entryPoints: ["src/extension.ts"],
  bundle: true,
  outfile: "out/extension.js",
  external: ["vscode"],
  format: "cjs",
  platform: "node",
  sourcemap: true,
  minify: false,
};

async function main() {
  if (watch) {
    const ctx = await esbuild.context(options);
    await ctx.watch();
    console.log("watching...");
  } else {
    await esbuild.build(options);
  }
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
