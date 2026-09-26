// ABI JSON是唯一来源；此生成器不解析Rust/AS程序源码。
import { readFileSync, writeFileSync } from "node:fs";
const root = new URL("../", import.meta.url);
const abi = JSON.parse(readFileSync(new URL("themes/wasm/abi.json", root), "utf8"));
const fail = message => { throw new Error("Invalid WASM ABI: " + message); };
const identifier = name => typeof name === "string" && /^[A-Za-z_][A-Za-z0-9_]*$/.test(name);
const keys = (value, required) => {
  if (!value || typeof value !== "object" || Array.isArray(value) ||
      Object.keys(value).sort().join() !== [...required].sort().join()) fail("unexpected/missing properties");
};
keys(abi, ["version", "module", "doc", "enums", "functions"]);
if (!Number.isInteger(abi.version) || abi.version < 1 || !identifier(abi.module) ||
    !Array.isArray(abi.doc) || abi.doc.some(v => typeof v !== "string")) fail("header");
const types = { i32:["i32","i32","i32"],u32:["u32","u32","i32"],i64:["i64","i64","i64"],f32:["f32","f32","f32"],f64:["f64","f64","f64"],ptr:["*const u8","i32","i32"],mut_ptr:["*mut u8","i32","i32"] };
const unique = (values, label) => { if (new Set(values).size !== values.length) fail("duplicate " + label); };
if (!Array.isArray(abi.enums) || !Array.isArray(abi.functions)) fail("declaration arrays");
unique(abi.enums.map(e=>e.name), "enum");
unique(abi.functions.map(f=>f.name), "function");
for (const e of abi.enums) {
  keys(e, ["name","doc","values"]);
  if (!identifier(e.name) || typeof e.doc !== "string") fail("enum name/doc");
  if (!e.values || typeof e.values !== "object" || Array.isArray(e.values) || !Object.keys(e.values).length) fail("enum values");
  unique(Object.values(e.values), e.name + " value");
  for (const [name,value] of Object.entries(e.values)) {
    if (!identifier(name) || !Number.isInteger(value) || value < -2147483648 || value > 2147483647) fail(e.name + "." + name);
  }
}
for (const f of abi.functions) {
  keys(f, ["name","doc","params","result","group"]);
  if (!["draw","view","resources","surface","interaction","config","diagnostics","animation","layers"].includes(f.group)) fail("function group " + f.name);
  if (!identifier(f.name) || typeof f.doc !== "string" || !Array.isArray(f.params) ||
      !(f.result === "void" || ["i32","u32","i64","f32","f64"].includes(f.result))) fail("function " + f.name);
  unique(f.params.map(p=>p.name), f.name+" argument");
  for (const p of f.params) {
    keys(p, ["name","type"]);
    if (!identifier(p.name) || !Object.hasOwn(types,p.type)) fail(f.name+" argument "+p.name);
  }
}
const header = "// Generated from themes/wasm/abi.json. DO NOT EDIT.\n";
const doc = (text,prefix) => text ? text.split("\n").map(line=>prefix+line).join("\n")+"\n" : "";
const rsTypes = header+"#![allow(dead_code)]\n"+
  "//! 与宿主 ABI 对齐的版本号、字段编号和语义枚举。\n"+
  "//!\n"+
  "//! 这些枚举的 `repr(i32)` 数值属于 ABI 合约；主题应使用变体而非自行假定编号。\n"+
  "/// 当前主题 ABI 版本；导出 `theme_abi_version` 时返回此值。\n"+
  `pub const ABI_VERSION: i32 = ${abi.version};\n`+
  abi.enums.map(e=>doc(e.doc,"/// ")+
    "#[repr(i32)]\n#[derive(Debug, Clone, Copy, PartialEq, Eq)]\n"+
    `pub enum ${e.name} {\n`+Object.entries(e.values).map(([n,v])=>`    ${n} = ${v},`).join("\n")+"\n}\n"+
    `impl TryFrom<i32> for ${e.name} {\n    type Error = i32;\n    fn try_from(value: i32) -> Result<Self, i32> {\n        match value {\n`+
    Object.entries(e.values).map(([n,v])=>`            ${v} => Ok(Self::${n}),`).join("\n")+
    "\n            _ => Err(value),\n        }\n    }\n}\n").join("\n");
const asTypes = header+`export const ABI_VERSION:i32 = ${abi.version};\n`+
  abi.enums.map(e=>doc(e.doc,"// ")+`export enum ${e.name} {\n`+
    Object.entries(e.values).map(([n,v])=>`  ${n} = ${v},`).join("\n")+"\n}\n").join("\n");
const rs = header+abi.doc.map(t=>doc(t,"//! ")).join("")+
  `#[link(wasm_import_module = "${abi.module}")]\nunsafe extern "C" {\n`+
  abi.functions.map(f=>doc(`分组：${f.group}。${f.doc}`,"    /// ")+`    pub fn ${f.name}(${f.params.map(p=>p.name+": "+types[p.type][0]).join(", ")})${f.result==="void"?"":" -> "+types[f.result][0]};\n`).join("")+"}\n";
const as = header+abi.functions.map(f=>doc(`分组：${f.group}。${f.doc}`,"// ")+`@external("${abi.module}", "${f.name}")\nexport declare function ${f.name}(${f.params.map(p=>p.name+": "+types[p.type][1]).join(", ")}): ${f.result};\n`).join("\n");
const signatures = header+"/// 由 `themes/wasm/abi.json` 生成的 ABI 导入签名表，供宿主校验主题模块。\n"+
  "#[rustfmt::skip]\npub const IMPORTS: &[(&str, &[&str], &[&str])] = &[\n"+
  abi.functions.map(f=>`    ("${f.name}", &[${f.params.map(p=>JSON.stringify(types[p.type][2])).join(", ")}], &[${f.result==="void"?"":JSON.stringify(types[f.result][2])}]),`).join("\n")+"\n];\n";
const js = header+abi.enums.map(e=>`export const ${e.name} = Object.freeze(${JSON.stringify(e.values)});\n`).join("");
for (const [path,content] of [
  ["themes/wasm/sdk-rust/src/raw.rs",rs],
  ["themes/wasm/sdk-rust/src/types.rs",rsTypes],
  ["themes/wasm/sdk-as/assembly/raw.ts",as],
  ["themes/wasm/sdk-as/assembly/types.ts",asTypes],
  ["themes/wasm/src/abi_generated.rs",signatures],
  ["themes/wasm/sdk-as/test/abi.mjs",js],
]) {
  const url = new URL(path,root);
  if (process.argv.includes("--check")) {
    if (readFileSync(url,"utf8").replaceAll("\r\n","\n")!==content) throw new Error("Stale WASM ABI: "+path);
  } else writeFileSync(url,content);
}
