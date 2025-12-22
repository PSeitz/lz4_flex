const fs = require('fs');
const path = require('path');

async function loadWasm(wasmPath) {
    const wasmBuffer = fs.readFileSync(wasmPath);
    const { instance } = await WebAssembly.instantiate(wasmBuffer, {});
    return instance;
}

function generateTestData(size) {
    // Compressible JSON-like data
    const pattern = '{"id":12345,"name":"test_user","email":"user@example.com","values":[1,2,3,4,5],"nested":{"a":1,"b":2}}';
    const data = Buffer.alloc(size);
    for (let i = 0; i < size; i++) {
        data[i] = pattern.charCodeAt(i % pattern.length);
    }
    return data;
}

function benchmark(instance, data, runs = 10) {
    const memory = new Uint8Array(instance.exports.memory.buffer);
    const inputPtr = 1024 * 1024; // Offset into memory
    
    // Copy data to WASM memory
    memory.set(data, inputPtr);
    
    // Warmup
    for (let i = 0; i < 3; i++) {
        instance.exports.compress_block(inputPtr, data.length);
    }
    
    const times = [];
    let compressedLen = 0;
    
    for (let i = 0; i < runs; i++) {
        const start = performance.now();
        compressedLen = instance.exports.compress_block(inputPtr, data.length);
        times.push(performance.now() - start);
    }
    
    times.sort((a, b) => a - b);
    const median = times[Math.floor(times.length / 2)];
    
    return { median, compressedLen };
}

function benchmarkDecompress(instance, compressedData, originalSize, runs = 10) {
    const memory = new Uint8Array(instance.exports.memory.buffer);
    const inputPtr = 1024 * 1024;
    
    memory.set(compressedData, inputPtr);
    
    // Warmup
    for (let i = 0; i < 3; i++) {
        instance.exports.decompress_block(inputPtr, compressedData.length, originalSize);
    }
    
    const times = [];
    for (let i = 0; i < runs; i++) {
        const start = performance.now();
        instance.exports.decompress_block(inputPtr, compressedData.length, originalSize);
        times.push(performance.now() - start);
    }
    
    times.sort((a, b) => a - b);
    return times[Math.floor(times.length / 2)];
}

async function main() {
    const baseInstance = await loadWasm(path.join(__dirname, 'lz4_block_base.wasm'));
    const simdInstance = await loadWasm(path.join(__dirname, 'lz4_block_simd.wasm'));
    
    // Verify both work
    console.log('Base roundtrip:', baseInstance.exports.test_roundtrip() === 1 ? 'PASS' : 'FAIL');
    console.log('SIMD roundtrip:', simdInstance.exports.test_roundtrip() === 1 ? 'PASS' : 'FAIL');
    
    console.log('\n🔬 LZ4 Block API: SIMD vs Base Benchmark\n');
    
    const sizes = [
        { name: '10 KB', size: 10 * 1024 },
        { name: '100 KB', size: 100 * 1024 },
        { name: '1 MB', size: 1024 * 1024 },
    ];
    
    console.log('=== COMPRESSION ===');
    console.log('| Size | Base (ms) | SIMD (ms) | Base Speed | SIMD Speed | Speedup |');
    console.log('|------|-----------|-----------|------------|------------|---------|');
    
    for (const { name, size } of sizes) {
        const data = generateTestData(size);
        const sizeMB = size / 1024 / 1024;
        
        const baseResult = benchmark(baseInstance, data);
        const simdResult = benchmark(simdInstance, data);
        
        const baseSpeed = (sizeMB / (baseResult.median / 1000)).toFixed(1);
        const simdSpeed = (sizeMB / (simdResult.median / 1000)).toFixed(1);
        const speedup = (baseResult.median / simdResult.median).toFixed(2);
        
        console.log(`| ${name} | ${baseResult.median.toFixed(3)} | ${simdResult.median.toFixed(3)} | ${baseSpeed} MB/s | ${simdSpeed} MB/s | ${speedup}x |`);
    }
    
    console.log('\n=== DECOMPRESSION ===');
    console.log('| Size | Base (ms) | SIMD (ms) | Base Speed | SIMD Speed | Speedup |');
    console.log('|------|-----------|-----------|------------|------------|---------|');
    
    for (const { name, size } of sizes) {
        const data = generateTestData(size);
        const sizeMB = size / 1024 / 1024;
        
        // Get compressed data from base (same for both)
        const memory = new Uint8Array(baseInstance.exports.memory.buffer);
        const inputPtr = 1024 * 1024;
        memory.set(data, inputPtr);
        const compressedLen = baseInstance.exports.compress_block(inputPtr, data.length);
        const outputPtr = baseInstance.exports.get_output_ptr();
        const compressedData = Buffer.from(memory.slice(outputPtr, outputPtr + compressedLen));
        
        const baseTime = benchmarkDecompress(baseInstance, compressedData, size);
        const simdTime = benchmarkDecompress(simdInstance, compressedData, size);
        
        const baseSpeed = (sizeMB / (baseTime / 1000)).toFixed(1);
        const simdSpeed = (sizeMB / (simdTime / 1000)).toFixed(1);
        const speedup = (baseTime / simdTime).toFixed(2);
        
        console.log(`| ${name} | ${baseTime.toFixed(3)} | ${simdTime.toFixed(3)} | ${baseSpeed} MB/s | ${simdSpeed} MB/s | ${speedup}x |`);
    }
}

main().catch(console.error);

