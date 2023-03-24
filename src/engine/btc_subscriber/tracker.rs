use std::str::FromStr;

use anyhow::Result;
use miniscript::bitcoin::secp256k1::All;
use miniscript::bitcoin::secp256k1::Secp256k1;
use miniscript::{
    bitcoin::{util::bip32::ExtendedPubKey, Script},
    Descriptor, DescriptorPublicKey,
};

pub struct AddressTracker {
    secp: Secp256k1<All>,
    g0_xpub: ExtendedPubKey,
    g1_xpub: ExtendedPubKey,
    g2_xpub: ExtendedPubKey,
}

impl AddressTracker {
    pub const LEN: usize = 3;

    pub fn new(xpubs: &[String; AddressTracker::LEN]) -> Result<AddressTracker> {
        let secp = Secp256k1::new();

        if xpubs.len() != AddressTracker::LEN {
            todo!()
        }

        let g0_xpub = ExtendedPubKey::from_str(&xpubs[0])?;
        let g1_xpub = ExtendedPubKey::from_str(&xpubs[1])?;
        let g2_xpub = ExtendedPubKey::from_str(&xpubs[2])?;

        Ok(AddressTracker {
            secp,
            g0_xpub,
            g1_xpub,
            g2_xpub,
        })
    }

    pub fn generate_script_pubkey(
        &self,
        master_xpub: ExtendedPubKey,
        index: u32,
    ) -> Result<Script> {
        let s = format!(
            "wsh(t:or_c(pk({}),v:multi(1,{}/1/0/*,{},{})))",
            master_xpub.to_pub(),
            self.g0_xpub.to_pub(),
            self.g1_xpub.to_pub(),
            self.g2_xpub.to_pub()
        );

        let desc: Descriptor<DescriptorPublicKey> = Descriptor::from_str(s.as_str())?;
        let bridge_descriptor = desc
            .at_derivation_index(index)
            .derived_descriptor(&self.secp)?;

        Ok(bridge_descriptor.script_pubkey())
    }
}
