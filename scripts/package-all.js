const { SUPPORTED, copyBinary } = require('./copy-bin');

const triples = Object.keys(SUPPORTED).sort();

let copied = 0;
let missing = 0;
for (const triple of triples) {
  if (copyBinary(triple)) {
    copied++;
  } else {
    missing++;
    console.log(`[SKIP] No release binary for ${triple}`);
  }
}

console.log(`\nPrepared ${copied} platform package(s); ${missing} not built on this machine.`);
console.log('To build them, add the targets via `rustup target add <triple>` and run `npm run build`.');