const fs = require('fs');
const path = require('path');

async function main() {
    const wasmPath = path.join(__dirname, 'target/wasm32-unknown-unknown/release/lz4_flex_wasm_test.wasm');
    const wasmBuffer = fs.readFileSync(wasmPath);
    
    // We need a memory import for the allocator
    const memory = new WebAssembly.Memory({ initial: 256 });
    
    const { instance } = await WebAssembly.instantiate(wasmBuffer, {
        env: { memory }
    });
    
    const result = instance.exports.test_roundtrip();
    console.log('Roundtrip test result:', result === 1 ? 'PASS' : 'FAIL');
}

main().catch(console.error);
