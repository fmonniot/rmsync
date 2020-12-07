use serde::Deserialize;

pub enum TokenError {
    Crypto(CryptoError),
    Json(serde_json::Error),
}


impl From<CryptoError> for TokenError {
    fn from(e: CryptoError) -> Self {
        TokenError::Crypto(e)
    }
}

impl From<serde_json::Error> for TokenError {
    fn from(error: serde_json::Error) -> Self {
        TokenError::Json(error)
    }
}

#[derive(Debug, Deserialize)]
pub struct Token {
    access_token: String,
    refresh_token: String,
    scope: String,
    token_type: String,
    id_token: String,
    expiry_date: u32,
}

pub struct TokenManager {
    crypto: Cryptographer,
}

impl TokenManager {
    pub async fn fetch_by_email(&self, email: &str) -> Result<Token, TokenError> {
        let encrypted_blob = self.fetch_from_datastore(email).await?;

        let blob = self.crypto.decrypt(&encrypted_blob)?;

        let token = serde_json::from_slice(&blob)?;
        
        Ok(token)
    }

    async fn fetch_from_datastore(&self, email: &str) -> Result<String, TokenError> {
        // TODO Add Google Cloud Datastore call here
        Ok("".to_string())
    }
}


#[derive(Debug)]
pub enum CryptoError {
    Base64(base64::DecodeError),
    Decryption,
}

impl From<std::env::VarError> for CryptoError {
    fn from(_error: std::env::VarError) -> Self {
        todo!("var")
    }
}

impl From<base64::DecodeError> for CryptoError {
    fn from(e: base64::DecodeError) -> Self {
        CryptoError::Base64(e)
    }
}

pub struct Cryptographer {
    key: Key,
}

const NONCE_LENGTH: usize = 24;

use sodiumoxide::crypto::secretbox::{self, Nonce, Key};

impl Cryptographer {

    pub fn from_env() -> Result<Cryptographer, CryptoError> {
        let key = std::env::var("CRYPTO_KEY")?;

        Cryptographer::new(&key)
    }

    pub fn new(key: &str) -> Result<Cryptographer, CryptoError> {
        let key_bytes = key_to_bytes(key)?;
        let key = Key::from_slice(&key_bytes).expect("can't create key");

        Ok(Cryptographer {
            key
        })
    }

    pub fn decrypt(&self, cipher: &str) -> Result<Vec<u8>, CryptoError> {
        let msg_nonce = base64::decode(cipher)?;

        let nonce = Nonce::from_slice(&msg_nonce[..NONCE_LENGTH]).expect("not sure what to expect actually");
        let msg = &msg_nonce[NONCE_LENGTH..];

        let vec = secretbox::open(msg, &nonce, &self.key).map_err(|_| CryptoError::Decryption)?;

        Ok(vec)
    }
}

use sha2::{Sha256, Digest};

fn key_to_bytes(key: &str) -> Result<Vec<u8>, base64::DecodeError> {
    let mut hasher = Sha256::new();
    hasher.update(key);
    let result = hasher.finalize();

    let key = format!("{}=", &hex::encode(result)[..43]);

    base64::decode_config(key.as_bytes(), base64::STANDARD.decode_allow_trailing_bits(true))
}

#[cfg(test)]
mod tests {
    use super::Cryptographer;

    // Constant obtained on the javascript side, to verify we can decode what
    // the cloud function will encode
    const KEY: &str = "nsauiteusanits";
    const SECRET: &str = "faristerst";
    const CIPHER: &str = "ue5MLiZuG4mwvGqovlCOlzPb30M9eQK/WD+MZ4PYJDLcJq5chwfYD4yQxAGN1/mfwQQ=";

    #[test]
    fn decrypt_known_secret() {
        let c = Cryptographer::new(KEY).unwrap();

        let res = c.decrypt(CIPHER).unwrap();

        assert_eq!(res, SECRET.as_bytes());
    }
}