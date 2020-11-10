use std::collections::HashMap;
use std::fmt;
use std::fmt::{Debug, Formatter};
use std::fs::File;
use std::io::{Read, Write};
use std::num::NonZeroU32;
use std::path::Path;

use anyhow::Error;
use base64::{decode, encode};
use pem::{parse_many, Pem};
use rand::prelude::*;
use ring::{digest, pbkdf2};
use secp256k1::{Message, PublicKey, SecretKey, Signature};
use secstr::{SecStr, SecVec};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
// use hex::{FromHex, ToHex};

use sodiumoxide::crypto::secretbox;
use sodiumoxide::crypto::secretbox::{Key, Nonce};

const CREDENTIAL_LEN: usize = digest::SHA256_OUTPUT_LEN;
const N_ITER: NonZeroU32 = unsafe { NonZeroU32::new_unchecked(100_000) }; //todo tune len

#[derive(Eq, PartialEq, Debug)]
pub struct KeyData {
    eth: EthSigner,
    ton: TonSigner,
}

#[derive(Eq, PartialEq)]
struct EthSigner {
    pubkey: PublicKey,
    private_key: SecretKey,
}

#[derive(Eq, PartialEq, Clone)]
struct TonSigner {
    inner: Vec<u8>,
}

impl Debug for TonSigner {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "***SECRET***")
    }
}

impl Debug for EthSigner {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.pubkey)
    }
}

#[derive(Serialize, Deserialize)]
struct CryptoData {
    #[serde(
        serialize_with = "serialize_pubkey",
        deserialize_with = "deserialize_pubkey"
    )]
    eth_pubkey: PublicKey,
    #[serde(serialize_with = "buffer_to_hex", deserialize_with = "hex_to_buffer")]
    salt: Vec<u8>,
    #[serde(serialize_with = "buffer_to_hex", deserialize_with = "hex_to_buffer")]
    encrypted_ton_data: Vec<u8>,
    #[serde(serialize_with = "buffer_to_hex", deserialize_with = "hex_to_buffer")]
    encrypted_private_key: Vec<u8>,
    #[serde(
        serialize_with = "serialize_nonce",
        deserialize_with = "deserialize_nonce"
    )]
    eth_nonce: Nonce,
    #[serde(
        serialize_with = "serialize_nonce",
        deserialize_with = "deserialize_nonce"
    )]
    ton_nonce: Nonce,
}

/// Serializes `buffer` to a lowercase hex string.
pub fn buffer_to_hex<T, S>(buffer: &T, serializer: S) -> Result<S::Ok, S::Error>
where
    T: AsRef<[u8]> + ?Sized,
    S: Serializer,
{
    serializer.serialize_str(&*encode(&buffer.as_ref()))
}

/// Deserializes a lowercase hex string to a `Vec<u8>`.
pub fn hex_to_buffer<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::Error;
    String::deserialize(deserializer)
        .and_then(|string| decode(string).map_err(|e| D::Error::custom(e.to_string())))
}

fn serialize_pubkey<S>(t: &PublicKey, ser: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    buffer_to_hex(&t.serialize(), ser)
}

fn serialize_nonce<S>(t: &Nonce, ser: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    buffer_to_hex(&t[..], ser)
}

fn deserialize_nonce<'de, D>(deser: D) -> Result<Nonce, D::Error>
where
    D: Deserializer<'de>,
{
    hex_to_buffer(deser).and_then(|x| {
        Nonce::from_slice(&*x).ok_or_else(|| serde::de::Error::custom("Failed deserializing nonce"))
    })
}

fn deserialize_pubkey<'de, D>(deser: D) -> Result<PublicKey, D::Error>
where
    D: Deserializer<'de>,
{
    hex_to_buffer(deser).and_then(|x| {
        PublicKey::from_slice(&*x).map_err(|e| serde::de::Error::custom(e.to_string()))
    })
}

impl EthSigner {
    pub fn sign(&self, data: &[u8]) -> Result<Signature, Error> {
        use sha3::{Digest, Keccak256};
        let mut eth_data: Vec<u8> = b"\x19Ethereum Signed Message:\n".to_vec();
        eth_data.extend_from_slice(data.len().to_string().as_bytes());
        eth_data.extend_from_slice(&data);
        let hash = Keccak256::digest(&eth_data);
        let message = Message::from_slice(&*hash)?;
        let secp = secp256k1::Secp256k1::new();
        Ok(secp.sign(&message, &self.private_key))
    }
}

impl KeyData {
    pub fn from_file<T>(path: T, password: SecStr) -> Result<Self, Error>
    where
        T: AsRef<Path>,
    {
        let mut file = File::open(&path)?;
        let mut content = Vec::new();
        file.read_to_end(&mut content)?;
        let parsed = parse_many(&content);
        let pem_map = parsed
            .into_iter()
            .fold(HashMap::new(), |mut map: HashMap<_, _>, pem| {
                map.insert(pem.tag, pem.contents);
                map
            });
        let eth_nonce = Nonce::from_slice(pem_map.get("eth_nonce").expect("No nonce in pem file"))
            .expect("Bad nonce eth provided");
        let salt = pem_map.get("salt").expect("No salt in pem file").clone();
        let pubkey = PublicKey::from_slice(pem_map.get("pubkey").expect("No pubkey in map"))?;
        let encrypted_private_key = pem_map
            .get("encrypted_private_key")
            .expect("No encrypted_keypair in pem file");
        let encrypted_ton_data = pem_map
            .get("encrypted_ton_data")
            .expect("No encrypted_ton_data in pem file");
        let ton_nonce =
            Nonce::from_slice(pem_map.get("ton_nonce").expect("No ton_nonce in pem file"))
                .expect("Bad nonce for ton provided");

        let sym_key = Self::key_from_password(password, &*salt);
        let private_key =
            Self::private_key_from_encrypted(&*encrypted_private_key, &sym_key, &eth_nonce);
        let ton_data = secretbox::open(encrypted_ton_data, &ton_nonce, &sym_key)
            .expect("Failed decrypting ton secret data");
        Ok(Self {
            eth: EthSigner {
                pubkey,
                private_key,
            },
            ton: TonSigner { inner: ton_data },
        })
    }

    fn key_from_password(password: SecStr, salt: &[u8]) -> Key {
        let mut pbkdf2_hash = SecVec::new(vec![0; CREDENTIAL_LEN]);
        pbkdf2::derive(
            pbkdf2::PBKDF2_HMAC_SHA256,
            N_ITER,
            salt,
            password.unsecure(),
            &mut pbkdf2_hash.unsecure_mut(),
        );
        dbg!(&pbkdf2_hash.unsecure());
        secretbox::Key::from_slice(&pbkdf2_hash.unsecure()).expect("Shouldn't panic")
    }

    fn private_key_from_encrypted(encrypted_key: &[u8], key: &Key, nonce: &Nonce) -> SecretKey {
        SecretKey::from_slice(
            &secretbox::open(encrypted_key, nonce, key).expect("Failed decrypting eth SecretKey"),
        )
        .expect("Failed constructing SecretKey from decrypted data")
    }

    pub fn init<T>(pem_file_path: T, password: SecStr, ton_data: Vec<u8>) -> Result<Self, Error>
    //todo use Writer instead of Path?
    where
        T: AsRef<Path>,
    {
        sodiumoxide::init().expect("Failed initializing libsodium");
        let eth_nonce = secretbox::gen_nonce();
        let mut rng = rand::rngs::OsRng::new().expect("OsRng fail");
        let mut salt = vec![0u8; CREDENTIAL_LEN];
        rng.fill(salt.as_mut_slice());
        let key = Self::key_from_password(password, &salt);
        let curve = secp256k1::Secp256k1::new();
        let (private, public) = curve.generate_keypair(&mut rng);
        let encrypted_private_key = secretbox::seal(&private[..], &eth_nonce, &key);
        let pubkey = Vec::from(public.serialize());
        let eth_nonce_bytes = Vec::from(eth_nonce.0);
        let ton_nonce = secretbox::gen_nonce();
        let ton_nonce_bytes = Vec::from(ton_nonce.0);
        let encrypted_ton_data = secretbox::seal(&ton_data, &ton_nonce, &key);

        let pem_data = vec![
            Pem {
                tag: "pubkey".into(),
                contents: pubkey,
            },
            Pem {
                tag: "salt".into(),
                contents: salt,
            },
            Pem {
                tag: "encrypted_private_key".into(),
                contents: encrypted_private_key,
            },
            Pem {
                tag: "eth_nonce".into(),
                contents: eth_nonce_bytes,
            },
            Pem {
                tag: "ton_nonce".into(),
                contents: ton_nonce_bytes,
            },
            Pem {
                tag: "encrypted_ton_data".into(),
                contents: encrypted_ton_data,
            },
        ];

        let serialized_self = pem::encode_many(&pem_data);
        let mut pem_file = File::create(pem_file_path)?;
        pem_file.write_all(serialized_self.as_bytes())?;
        Ok(Self {
            eth: EthSigner {
                private_key: private,
                pubkey: public,
            },
            ton: TonSigner { inner: ton_data },
        })
    }
}

#[cfg(test)]
mod test {
    
    
    use secstr::SecStr;

    use crate::key_managment::{KeyData};

    // #[test]
    // fn test_sign() {
    //     use hex::decode;
    //     let message_text = b"Some data";
    //
    //     let signer = Signer::from_file("./test/test.keys")
    //     let res = signer.sign(b"test").unwrap();
    //     assert_eq!(res, Signature::from_bytes(decode("2d34bd34780a6fb76181c103653161c456eb7163eb9e098b29fc1619b46fa123ae3d962ca738e480a83ed1943e965b652175c78536781abe426f46cdfe64a209").unwrap().as_slice()).unwrap());
    // }
    #[test]
    fn test_init() {
        let password = SecStr::new("123".into());
        let path = "./test/test_init.key";
        let signer =
            KeyData::init(&path, password.clone(), "SOME_SUPA_SECRET_DATA".into()).unwrap();
        let read_signer = KeyData::from_file(&path, password).unwrap();
        std::fs::remove_file(path).unwrap();
        assert_eq!(read_signer, signer);
    }

    #[test]
    fn test_bad_password() {
        let password = SecStr::new("123".into());
        let path = "./test/test_bad.key";
        KeyData::init(&path, password.clone(), "SOME_SUPA_SECRET_DATA".into()).unwrap();
        let result =
            std::panic::catch_unwind(|| KeyData::from_file(&path, SecStr::new("lol".into())));
        // std::fs::remove_file(path);
        assert!(result.is_err());
    }

    #[test]
    fn test_size() {
        assert_eq!(
            sodiumoxide::crypto::secretbox::xsalsa20poly1305::KEYBYTES,
            ring::digest::SHA256_OUTPUT_LEN
        );
    }
}
