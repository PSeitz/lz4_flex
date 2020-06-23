import * as wasm from "lz4-wasm";

// use TextEncoder tro get bytes from string
var enc = new TextEncoder();
const compressed = wasm.compress(enc.encode("compress this text, compress this text pls. thx. thx. thx. thx. thx"));
const original = wasm.decompress(compressed);

var dec = new TextDecoder("utf-8");
alert(dec.decode(original))

