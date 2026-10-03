// Run the wasm32-wasip1 build of eve-dogma under Node's WASI (no extra tools):
//   cargo build --release --target wasm32-wasip1
//   node wasm/run.mjs <dataset.json.gz> calc < request.json
// Arguments after the dataset are passed to the CLI unchanged (calc, batch, serve-stdio, eft, search, type, meta).
// The dataset's directory is preopened read-only as /data; nothing else on the host is visible to the module.
import { readFileSync, realpathSync } from "node:fs";
import { WASI } from "node:wasi";
import { argv, exit } from "node:process";
import { basename, dirname, resolve, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const wasmPath = process.env.EVE_DOGMA_WASM ?? join(here, "..", "target", "wasm32-wasip1", "release", "eve-dogma.wasm");
const [dataset, ...rest] = argv.slice(2);
if (!dataset) {
  console.error("usage: node wasm/run.mjs <dataset.json.gz> <command> [args...]");
  exit(2);
}
const ds = realpathSync(resolve(dataset)); // a symlinked dataset is opened at its real location
const wasi = new WASI({
  version: "preview1",
  args: ["eve-dogma", "--dataset", `/data/${basename(ds)}`, ...rest],
  env: { EVE_DOGMA_NO_CACHE: "1" },
  preopens: { "/data": dirname(ds) },
  returnOnExit: true,
});
const { instance } = await WebAssembly.instantiate(readFileSync(wasmPath), wasi.getImportObject());
exit(wasi.start(instance));
