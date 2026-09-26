import { copyFileSync, mkdirSync } from "node:fs";
import { resolve } from "node:path";

const moduleName = process.argv[2];
if (!moduleName || !/^[a-z0-9_-]+$/i.test(moduleName)) {
  throw new Error("Usage: node copy-assets.mjs <module-name>");
}

const source = resolve("assets");
const destination = resolve("build", `${moduleName}.assets`);
mkdirSync(destination, { recursive: true });
for (const name of ["zh.png", "en.png"]) {
  copyFileSync(resolve(source, name), resolve(destination, name));
}
