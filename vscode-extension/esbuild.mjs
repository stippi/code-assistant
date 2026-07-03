import esbuild from "esbuild";

const watch = process.argv.includes("--watch");

const contexts = await Promise.all([
  esbuild.context({
    entryPoints: ["src/extension.ts"],
    bundle: true,
    platform: "node",
    format: "cjs",
    target: "node20",
    outfile: "dist/extension.js",
    external: ["vscode"],
    sourcemap: true,
  }),
  esbuild.context({
    entryPoints: ["webview/main.ts"],
    bundle: true,
    platform: "browser",
    format: "iife",
    target: "es2022",
    outfile: "dist/webview.js",
    sourcemap: true,
  }),
]);

if (watch) {
  await Promise.all(contexts.map((c) => c.watch()));
  console.log("watching…");
} else {
  await Promise.all(contexts.map((c) => c.rebuild()));
  await Promise.all(contexts.map((c) => c.dispose()));
}
