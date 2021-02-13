import * as wasm from "lz4-wasm";
// import * as JSZip from "jszip";
var lz4js = require('lz4/lib/binding')
import * as fflate from 'fflate/esm/browser.js';

import test_input_66k_JSON from '../../benches/compression_66k_JSON.txt';
import test_input_65k from '../../benches/compression_65k.txt';
import test_input_34k from '../../benches/compression_34k.txt';
import test_input_1k from '../../benches/compression_1k.txt';

function addText(text) {
    // let body = document.querySelectorAll('body');
    var div = document.createElement("div");
    div.innerHTML = text;

    div.style["font-size"] = "40px";

    document.getElementById("body").appendChild(div);
}
// function addRow(col1, col2, col3) {
//     // 
//     var tr = document.createElement("tr");
//     tr.style["font-size"] = "40px";

//     var td = document.createElement("td");
//     td.innerHTML = col1;
//     tr.appendChild(td);

//     var td = document.createElement("td");
//     td.innerHTML = col2;
//     tr.appendChild(td);

//     var td = document.createElement("td");
//     td.innerHTML = col3;
//     tr.appendChild(td);

//     document.querySelectorAll('tbody')[0].appendChild(tr);
// }

// async function benchmark_jszip_compression(argument) {

//     let total_bytes = 0;
//     var time0 = performance.now();
//     for (let i = 0; i < 1000; i++) {
//         var zip = new JSZip();
//         zip.file("a", test_input);

//         await zip.generateAsync({type: "uint8array"}).then(function (u8) {
//             // ...
//         });

//         total_bytes += test_input.length;
//     }

//     var time_in_ms = performance.now() - time0;

//     let total_mb = total_bytes / 1000000;
//     let time_in_s = time_in_ms / 1000;

// }

function sleep(ms) {
  return new Promise(resolve => setTimeout(resolve, ms));
}


const compressor = [
    {
        name: "lz4 wasm",
        prepareInput: function(input) {
            return new TextEncoder().encode(input)
        },
        compress: function(input) {
            return wasm.compress(input);
        },
        decompress: function(compressed, originalSize) {
            const original = wasm.decompress(compressed);
            return original;
        }
    },
    {
        name: "lz4 js",
        prepareInput: function(input) {
            return Buffer.from(input)
        },
        compress: function(input) {
            var output = Buffer.alloc( lz4js.compressBound(input.length) )
            var compressedSize = lz4js.compress(input, output)
            output = output.slice(0, compressedSize)
            return output;
        },
        decompress: function(compressed, originalSize) {
            var uncompressed = Buffer.alloc(originalSize)
            var uncompressedSize = lz4js.uncompress(compressed, uncompressed)
            uncompressed = uncompressed.slice(0, uncompressedSize)
            return uncompressed;
        }
    },
    {
        name: "fflate",
        prepareInput: function(input) {
            return Buffer.from(input)
        },
        compress: function(input) {
            return fflate.zlibSync(input, { level: 1 });
        },
        decompress: function(compressed, originalSize) {
            return fflate.unzlibSync(compressed);
        }
    }
]

let inputs = [
    {
        name: "66k_JSON",
        data: test_input_66k_JSON
    },
    {
        name: "65k Text",
        data: test_input_65k
    },
    {
        name: "34k Text",
        data: test_input_34k
    },
    {
        name: "1k Text",
        data: test_input_1k
    },
]

async function bench_maker() {
    addText("Starting Benchmark..")
    await sleep(10)
    for (const input of inputs) {
        addText("Input: " + input.name)
        for (const el of compressor) {
        
            bench_compression(el, input.data);
            await sleep(10)
            bench_decompression(el, input.data);
            await sleep(10)
        }
    }
    addText("Finished")
}

async function bench_compression(compressor, input) {

    const test_input_bytes = compressor.prepareInput(input)
    const compressed = compressor.compress(test_input_bytes);
    let total_bytes = 0;
    var time0 = performance.now();
    for (let i = 0; i < 1000; i++) {
        const compressed = compressor.compress(test_input_bytes);
        total_bytes += test_input_bytes.length;
        if(performance.now() - time0 > 3000){
            break;
        }
    }

    var time_in_ms = performance.now() - time0;

    let total_mb = total_bytes / 1000000;
    let time_in_s = time_in_ms / 1000;

    addText(compressor.name + " compression: " + (total_mb / time_in_s).toFixed(2)  + "MB/s" + " Ratio: " + (compressed.length / test_input_bytes.length).toFixed(2) )

}
async function bench_decompression(compressor, input) {

    const test_input_bytes = compressor.prepareInput(input)
    const compressed = compressor.compress(test_input_bytes);
    let total_bytes = 0;
    var time0 = performance.now();
    for (let i = 0; i < 1000; i++) {
        compressor.decompress(compressed, input.length);
        total_bytes += test_input_bytes.length;
        if(performance.now() - time0 > 3000){
            break;
        }
    }

    var time_in_ms = performance.now() - time0;

    let total_mb = total_bytes / 1000000;
    let time_in_s = time_in_ms / 1000;

    addText(compressor.name + " decompression: " + (total_mb / time_in_s).toFixed(2)  + "MB/s" + " Ratio: " + (compressed.length / test_input_bytes.length).toFixed(2) )
    
}

// run()
bench_maker();