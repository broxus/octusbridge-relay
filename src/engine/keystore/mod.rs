use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use nekoton_abi::*;
use nekoton_utils::TrustMe;
use secstr::SecUtf8;
use ton_types::UInt256;

use crate::config::{FromPhraseAndPath, StoredKeysData, UnencryptedEthData, UnencryptedTonData};
use crate::utils::*;

/// A collection of signers
pub struct KeyStore {
    pub eth: EthSigner,
    pub ton: TonSigner,
}

impl KeyStore {
    /// Loads and decrypts keystore state
    pub fn new<P>(keys_path: P, password: SecUtf8) -> Result<Arc<Self>>
    where
        P: AsRef<Path>,
    {
        let keys_path = keys_path.as_ref();
        let stored_data = if keys_path.exists() {
            log::info!("Using existing keys");
            StoredKeysData::load(keys_path)?
        } else {
            log::info!("Generating new keys");
            let data = StoredKeysData::new(
                password.unsecure(),
                UnencryptedEthData::generate()?,
                UnencryptedTonData::generate()?,
            )?;
            data.save(keys_path)?;
            data
        };

        let (eth_secret_key, ton_secret_key) =
            stored_data.decrypt_only_keys(password.unsecure())?;
        let eth_secret_key = secp256k1::SecretKey::from_slice(&eth_secret_key)?;
        let ton_secret_key = ed25519_dalek::SecretKey::from_bytes(&ton_secret_key)?;

        Ok(Arc::new(Self {
            eth: EthSigner::new(eth_secret_key),
            ton: TonSigner::new(ton_secret_key),
        }))
    }
}

pub struct EthSigner {
    secp256k1: secp256k1::Secp256k1<secp256k1::All>,
    secret_key: secp256k1::SecretKey,
    public_key: secp256k1::PublicKey,
    address: ethabi::Address,
}

impl EthSigner {
    fn new(secret_key: secp256k1::SecretKey) -> Self {
        let secp256k1 = secp256k1::Secp256k1::new();
        let public_key = secp256k1::PublicKey::from_secret_key(&secp256k1, &secret_key);
        let address = compute_eth_address(&public_key);

        Self {
            secp256k1,
            secret_key,
            public_key,
            address,
        }
    }

    /// Signs data according to https://eips.ethereum.org/EIPS/eip-191
    pub fn sign(&self, data: &[u8]) -> [u8; 65] {
        // 1. Calculate prefixed hash
        let data_hash = web3::signing::keccak256(data);
        let mut eth_data: Vec<u8> = "\x19Ethereum Signed Message:\n32".into();
        eth_data.extend_from_slice(&data_hash);

        // 2. Calculate hash of prefixed hash
        let hash = web3::signing::keccak256(&eth_data);
        let message = secp256k1::Message::from_slice(&hash).expect("Shouldn't fail");

        // 3. Sign
        let (id, signature) = self
            .secp256k1
            .sign_recoverable(&message, &self.secret_key)
            .serialize_compact();

        // 4. Prepare for ETH
        let mut ex_sign = [0u8; 65];
        ex_sign[..64].copy_from_slice(&signature);
        // recovery id with eth specific offset
        ex_sign[64] = id.to_i32() as u8 + 27;

        // Done
        ex_sign
    }

    pub fn secret_key(&self) -> &secp256k1::SecretKey {
        &self.secret_key
    }

    pub fn pubkey(&self) -> &secp256k1::PublicKey {
        &self.public_key
    }

    pub fn address(&self) -> &ethabi::Address {
        &self.address
    }
}

pub struct TonSigner {
    pair: ed25519_dalek::Keypair,
    public_key_bytes: UInt256,
}

impl TonSigner {
    fn new(secret_key: ed25519_dalek::SecretKey) -> Self {
        let public_key = ed25519_dalek::PublicKey::from(&secret_key);

        Self {
            pair: ed25519_dalek::Keypair {
                secret: secret_key,
                public: public_key,
            },
            public_key_bytes: UInt256::from(public_key.to_bytes()),
        }
    }

    pub fn public_key(&self) -> &UInt256 {
        &self.public_key_bytes
    }

    pub fn raw_public_key(&self) -> &ed25519_dalek::PublicKey {
        &self.pair.public
    }

    pub fn sign(&self, unsigned_message: &UnsignedMessage) -> Result<SignedMessage> {
        let time = chrono::Utc::now().timestamp_millis() as u64;
        let expire_at = (time / 1000) as u32 + MESSAGE_TTL_SEC;

        let headers = default_headers(time, expire_at, &self.pair.public);
        let body = unsigned_message.function.encode_input(
            &headers,
            &unsigned_message.inputs,
            false,
            Some(&self.pair),
        )?;

        let message = ton_block::Message::with_ext_in_header_and_body(
            ton_block::ExternalInboundMessageHeader {
                dst: unsigned_message.dst.clone(),
                ..Default::default()
            },
            body.into(),
        );

        Ok(SignedMessage {
            account: unsigned_message.account,
            message,
            expire_at,
        })
    }
}

pub struct UnsignedMessage {
    function: &'static ton_abi::Function,
    inputs: Vec<ton_abi::Token>,
    account: UInt256,
    dst: ton_block::MsgAddressInt,
}

impl UnsignedMessage {
    pub fn new(function: &'static ton_abi::Function, account: UInt256) -> Self {
        let dst = ton_block::MsgAddressInt::with_standart(None, 0, account.into()).trust_me();

        Self {
            function,
            inputs: Vec::with_capacity(function.inputs.len()),
            account,
            dst,
        }
    }

    pub fn arg<T>(mut self, arg: T) -> Self
    where
        T: BuildTokenValue,
    {
        let arg_name = self.function.inputs[self.inputs.len()].name.clone();
        self.inputs.push(arg.token_value().named(arg_name));
        self
    }

    pub fn build_without_signature(&self) -> Result<SignedMessage> {
        let time = chrono::Utc::now().timestamp_millis() as u64;
        let expire_at = (time / 1000) as u32 + MESSAGE_TTL_SEC;

        let headers = default_headers(time, expire_at, &Default::default());
        let body = self
            .function
            .encode_input(&headers, &self.inputs, false, None)?;

        let message = ton_block::Message::with_ext_in_header_and_body(
            ton_block::ExternalInboundMessageHeader {
                dst: self.dst.clone(),
                ..Default::default()
            },
            body.into(),
        );

        Ok(SignedMessage {
            account: self.account,
            message,
            expire_at,
        })
    }
}

#[derive(Debug, Clone)]
pub struct SignedMessage {
    pub account: UInt256,
    pub message: ton_block::Message,
    pub expire_at: u32,
}

fn default_headers(time: u64, expire_at: u32, public_key: &ed25519_dalek::PublicKey) -> HeadersMap {
    let mut header = HashMap::with_capacity(3);
    header.insert("time".to_string(), ton_abi::TokenValue::Time(time));
    header.insert("expire".to_string(), ton_abi::TokenValue::Expire(expire_at));
    header.insert(
        "pubkey".to_string(),
        ton_abi::TokenValue::PublicKey(Some(*public_key)),
    );
    header
}

type HeadersMap = HashMap<String, ton_abi::TokenValue>;

const MESSAGE_TTL_SEC: u32 = 60;

#[cfg(test)]
mod tst {
    use std::io::Write;
    use std::path::PathBuf;
    use std::str::FromStr;

    use tempfile::TempDir;

    use super::*;

    const TEST_PHRASE: &str =
        "spy taste penalty add aware trim crouch denial dinner arrest magic young";

    #[test]
    fn init() {
        let (_dir, path) = create_file();

        let eth = UnencryptedEthData::from_phrase(
            TEST_PHRASE.into(),
            UnencryptedEthData::DEFAULT_PATH.into(),
        )
        .unwrap();
        let ton = UnencryptedTonData::from_phrase(
            TEST_PHRASE.into(),
            UnencryptedTonData::DEFAULT_PATH.into(),
        )
        .unwrap();

        let data = StoredKeysData::new("lol", eth, ton).unwrap();
        data.save(path).unwrap();
    }

    const JSON: &str = r#"{
        "salt": "G+g0tWMEE0RkAKZq5MMuJOLT5Yw=",
        "eth": {
            "encrypted_seed_phrase": "qZ+9nriQ/HMWcIPL2nFDGYmDT12y/Z4SasoKK/86iqellvPrDjIqss1+5Hr26cVJtRMAieWqzwvyxa9ryXQ/zE/HY38lhXLfDNRplW7IxuUAwS5jp/npeA==",
            "encrypted_derivation_path": "t8DwiuveuTdUf8OJmyAAXUyzmmGAQBz3ykWpD2moS28=",
            "nonce": "db4f2c4b6a0abca6f77427e1"
        },
        "ton": {
            "encrypted_seed_phrase": "J0Mg2qJE6+Q0DJq/O2kqsVyJAkzeWVhqj9GkJrOBGE/uzg777q4zSzfDTfsqGYc4kHITaQdJBIu6DCVmo9ePpYJEoUBuew1zKAVe+kVMczebcQKXqrTEwA==",
            "encrypted_derivation_path": "ORxtzvEKq6lnC8Xqcid26hUXNpvckrijIEP+8YMgaVx8",
            "nonce": "dafbcbf23f77131d1e62a80b"
        }
    }"#;

    #[test]
    fn check_ok_passwd() {
        let (_dir, path) = create_file();
        let store = KeyStore::new(path, "lol".into()).unwrap();

        let expected_ton_key =
            hex::decode("6be37687497f5b54ffc9fec5c17e24be08e6cbcf8e240155b1735aa6da634183")
                .unwrap();
        let expected_eth_key =
            hex::decode("89d0fdd4e8ad43c60e5130741febe7c070e0e19223b011d99254fd2f0d206489")
                .unwrap();

        assert_eq!(store.ton.pair.secret.as_ref(), &expected_ton_key);
        assert_eq!(&store.eth.secret_key.as_ref()[..], &expected_eth_key);
    }

    #[test]
    fn check_bad_password() {
        let (_dir, path) = create_file();
        assert!(KeyStore::new(path, "kek".into()).is_err())
    }

    fn create_file() -> (TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.json");
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .open(&path)
            .unwrap();
        file.write_all(JSON.as_bytes()).unwrap();
        (dir, path)
    }

    fn default_keys() -> (secp256k1::SecretKey, ed25519_dalek::SecretKey) {
        let eth_private_key = secp256k1::SecretKey::from_slice(
            &hex::decode("416ddb82736d0ddf80cc50eda0639a2dd9f104aef121fb9c8af647ad8944a8b1")
                .unwrap(),
        )
        .unwrap();

        let ton_private_key = ed25519_dalek::SecretKey::from_bytes(
            &hex::decode("e371ef1d7266fc47b30d49dc886861598f09e2e6294d7f0520fe9aa460114e51")
                .unwrap(),
        )
        .unwrap();

        (eth_private_key, ton_private_key)
    }

    #[test]
    fn test_sign() {
        let message_text = b"hello_world1";

        let (private_key, _) = default_keys();
        let signer = EthSigner::new(private_key);
        let res = signer.sign(message_text);
        let expected = hex::decode("ff244ad5573d02bc6ead270d5ff48c490b0113225dd61617791ba6610ed1e56a007ec790f8fca53243907b888e6b33ad15c52fed3bc6a7ee5da2fa287ea4f8211b").unwrap();
        assert_eq!(expected.len(), res.len());
        assert_eq!(res, expected.as_slice());
    }

    #[test]
    fn test_address_derive() {
        let (key, _) = default_keys();
        let signer = EthSigner::new(key);
        let address = signer.address();
        let expected =
            ethabi::Address::from_str("9c5a095ae311cad1b09bc36ac8635f4ed4765dcf").unwrap();
        assert_eq!(address, &expected);
    }
}