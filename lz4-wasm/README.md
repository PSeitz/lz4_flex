<div align="center">

  <h1><code>lz4-wasm</code></h1>

  <strong>Extremely fast compression/decompression in the browser using wasm.</strong>

  <sub>Built with Rust</a></sub>
</div>


## 🚴 Usage


[**📚 Usage! 📚**][template-docs]

The wasm module exposes two function compress and decompress.
Both accept and return UInt8Array.


```

import * as wasm from "lz4-wasm";

// use TextEncoder to get bytes (UInt8Array) from string
var enc = new TextEncoder();
const compressed = wasm.compress(enc.encode("compress this text, compress this text pls. thx. thx. thx. thx. thx"));
const original = wasm.decompress(compressed);

var dec = new TextDecoder("utf-8");
alert(dec.decode(original))

```
