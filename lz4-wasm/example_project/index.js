import * as wasm from "lz4-wasm";
import * as JSZip from "jszip";


var enc = new TextEncoder();
// use TextEncoder to get bytes (UInt8Array) from string
const compressed = wasm.compress(enc.encode("compress this text, compress this text pls. thx. thx. thx. thx. thx"));
const original = wasm.decompress(compressed);

var dec = new TextDecoder("utf-8");
alert(dec.decode(original))


