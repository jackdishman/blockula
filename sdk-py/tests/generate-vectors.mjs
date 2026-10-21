// Public test seeds only. Regenerate after building @blockula/sdk:
// node sdk-py/tests/generate-vectors.mjs > sdk-py/tests/wallet-vectors.json
import { pathToFileURL } from 'node:url';
const sdk = process.argv[2] ? pathToFileURL(process.argv[2]) : new URL('../../sdk-ts/dist/index.js', import.meta.url);
const {Wallet, addressFromPublicKey, signingBytes, txId} = await import(sdk);
import {readFileSync} from 'node:fs';
const {ml_dsa65} = await import(new URL('../../node_modules/@noble/post-quantum/ml-dsa.js', sdk));
const shared = JSON.parse(readFileSync(new URL('../../sdk-ts/test/tx-vectors.json', import.meta.url)));
const cases = shared.cases.filter(c => ['transfer', 'register_name'].includes(c.tx.action.type));
const vectors = cases.map((c, i) => {
  const seed = new Uint8Array(32).fill(i + 1);
  const wallet = Wallet.fromSeed(seed);
  const signature = ml_dsa65.sign(signingBytes(c.tx), ml_dsa65.keygen(seed).secretKey, {extraEntropy:false});
  const signed = {...c.tx, public_key:Buffer.from(wallet.publicKey).toString('hex'), signature:Buffer.from(signature).toString('hex')};
  return {label:c.label,seed:Buffer.from(seed).toString('hex'),public_key:signed.public_key,
    key_id:wallet.keyId,address:addressFromPublicKey(wallet.publicKey),
    testnet_address:addressFromPublicKey(wallet.publicKey,'tboo'),tx:c.tx,
    signing_bytes:c.signing_bytes,signature:signed.signature,tx_id:txId(signed)};
});
console.log(JSON.stringify({source:'@blockula/sdk; deterministic ML-DSA-65, public test seeds only',vectors},null,2));
