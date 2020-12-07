// test, decrypt thing
const crypto = require('tweetnacl');
const {
  decodeUTF8,
  encodeUTF8,
  encodeBase64,
  decodeBase64,
} = require('tweetnacl-util');
const shajs = require('sha.js');

const NONCE_LENGTH = 24;

const decrypt = (ciphertext, TOKEN_ENCRYPTION_BYTES) => {
    const msgNonce = decodeBase64(ciphertext);

    const nonce = msgNonce.slice(0, NONCE_LENGTH);
    const msg = msgNonce.slice(NONCE_LENGTH, msgNonce.length);

    const decrypted = crypto.secretbox.open(
      msg,
      nonce,
      TOKEN_ENCRYPTION_BYTES
    );
    return decrypted
      ? encodeUTF8(decrypted)
      : "ERROR_TWEETNACL_DECRYPTION";
};

const encrypt = (data, TOKEN_ENCRYPTION_BYTES) => {
    const plaintext = decodeUTF8(data);
    const nonce = crypto.randomBytes(NONCE_LENGTH);

    const box = crypto.secretbox(
      plaintext,
      nonce,
      TOKEN_ENCRYPTION_BYTES
    );

    const msg = new Uint8Array(nonce.length + box.length);
    msg.set(nonce);
    msg.set(box, nonce.length);

    return encodeBase64(msg);
};

const secret = "faristerst"
const key = shajs('sha256')
    .update("nsauiteusanits")
    .digest('hex')
    .slice(0, 43) + '=';
console.log(key)
const cipher2 = encrypt(secret, decodeBase64(key))
console.log(cipher2)
const decoded = decrypt(cipher2, decodeBase64(key))
console.log(decoded)
