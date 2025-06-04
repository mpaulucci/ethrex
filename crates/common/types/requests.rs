use std::str::FromStr;

use bytes::Bytes;
use ethereum_types::Address;
use ethrex_rlp::{decode::RLPDecode, encode::RLPEncode, error::RLPDecodeError};
use k256::sha2::Sha256;
use keccak_hash::H256;
use lazy_static::lazy_static;
use serde::{Deserialize, Serialize};
use sha3::Digest;
use tracing::error;

use crate::serde_utils;

use super::{Bytes48, Receipt};

pub type Bytes32 = [u8; 32];
pub type Bytes96 = [u8; 96];
const DEPOSIT_TYPE: u8 = 0x00;
const WITHDRAWAL_TYPE: u8 = 0x01;
const CONSOLIDATION_TYPE: u8 = 0x02;

lazy_static! {
    static ref DEPOSIT_TOPIC: H256 =
        H256::from_str("649bbc62d0e31342afea4e5cd82d4049e7e1ee912fc0889aa790803be39038c5").unwrap();
}

#[derive(Clone, Debug)]
pub struct EncodedRequests(pub Bytes);

impl EncodedRequests {
    pub fn is_empty(&self) -> bool {
        self.0.len() <= 1
    }
}

impl<'de> Deserialize<'de> for EncodedRequests {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Ok(EncodedRequests(serde_utils::bytes::deserialize(
            deserializer,
        )?))
    }
}

impl Serialize for EncodedRequests {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serde_utils::bytes::serialize(&self.0, serializer)
    }
}

impl RLPEncode for EncodedRequests {
    fn encode(&self, buf: &mut dyn bytes::BufMut) {
        self.0.encode(buf)
    }
}

impl RLPDecode for EncodedRequests {
    fn decode_unfinished(rlp: &[u8]) -> Result<(Self, &[u8]), RLPDecodeError> {
        let (bytes, rest) = RLPDecode::decode_unfinished(rlp)?;
        Ok((EncodedRequests(bytes), rest))
    }
}

#[derive(Clone, Debug)]
pub enum Requests {
    Deposit(Vec<Deposit>),
    Withdrawal(Vec<u8>),
    Consolidation(Vec<u8>),
}

impl Requests {
    pub fn encode(&self) -> EncodedRequests {
        let bytes: Vec<u8> = match self {
            Requests::Deposit(deposits) => {
                let deposit_data = deposits.iter().flat_map(|d| d.to_summarized_byte_array());
                std::iter::once(DEPOSIT_TYPE).chain(deposit_data).collect()
            }
            Requests::Withdrawal(data) => std::iter::once(WITHDRAWAL_TYPE)
                .chain(data.iter().cloned())
                .collect(),
            Requests::Consolidation(data) => std::iter::once(CONSOLIDATION_TYPE)
                .chain(data.iter().cloned())
                .collect(),
        };

        EncodedRequests(Bytes::from(bytes))
    }

    /// Returns None if any of the deposit requests couldn't be parsed
    pub fn from_deposit_receipts(
        deposit_contract_address: Address,
        receipts: &[Receipt],
    ) -> Option<Requests> {
        let mut deposits = vec![];

        for r in receipts {
            for log in &r.logs {
                if log.address == deposit_contract_address
                    && log
                        .topics
                        .first()
                        .is_some_and(|topic| topic == &*DEPOSIT_TOPIC)
                {
                    deposits.push(Deposit::from_abi_byte_array(&log.data)?);
                }
            }
        }
        Some(Self::Deposit(deposits))
    }

    pub fn from_withdrawals_data(data: Vec<u8>) -> Requests {
        Requests::Withdrawal(data)
    }

    pub fn from_consolidation_data(data: Vec<u8>) -> Requests {
        Requests::Consolidation(data)
    }
}

#[derive(Debug, Clone)]
pub struct Deposit {
    pub pub_key: Bytes48,
    pub withdrawal_credentials: Bytes32,
    pub amount: u64,
    pub signature: Bytes96,
    pub index: u64,
}

// Followed and ported implementation from:
// https://github.com/lightclient/go-ethereum/blob/5c4d46f3614d26654241849da7dfd46b95eed1c6/core/types/deposit.go#L61
impl Deposit {
    pub fn from_abi_byte_array(data: &[u8]) -> Option<Deposit> {
        if data.len() != 576 {
            error!("Wrong data length when parsing deposit.");
            return None;
        }

        //Encoding scheme:
        //
        //positional arguments -> 5 parameters with uint256 positional value for each -> 160b
        //pub_key: 32b of len + 48b padded to 64b
        //withdrawal_credentials: 32b of len + 32b
        //amount: 32b of len + 8b padded to 32b
        //signature: 32b of len + 96b
        //index: 32b of len + 8b padded to 32b
        //
        //-> Total len: 576 bytes

        const OFFSET_LEN: usize = 32;

        const PUB_KEY_OFFSET: usize = 160;
        const WITHDRAWAL_CREDENTIALS_OFFSET: usize = 256;
        const AMOUNT_OFFSET: usize = 320;
        const SIGNATURE_OFFSET: usize = 384;
        const INDEX_OFFSET: usize = 512;

        const OFFSETS: [usize; 5] = [
            PUB_KEY_OFFSET,
            WITHDRAWAL_CREDENTIALS_OFFSET,
            AMOUNT_OFFSET,
            SIGNATURE_OFFSET,
            INDEX_OFFSET,
        ];

        const SIZE_LEN: usize = 32;

        const PUB_KEY_SIZE: usize = 48;
        const WITHDRAWAL_CREDENTIALS_SIZE: usize = 32;
        const AMOUNT_SIZE: usize = 8;
        const SIGNATURE_SIZE: usize = 96;
        const INDEX_SIZE: usize = 8;

        const SIZES: [usize; 5] = [
            PUB_KEY_SIZE,
            WITHDRAWAL_CREDENTIALS_SIZE,
            AMOUNT_SIZE,
            SIGNATURE_SIZE,
            INDEX_SIZE,
        ];

        // Compare two numbers with different byte count without padding
        let is_eq = |bytes: [u8; 32], u: usize| -> bool {
            bytes[..24] == [0; 24] && bytes[24..] == u.to_be_bytes()
        };

        // Validate Offsets & Sizes
        for (i, (expected_offset, expected_size)) in
            OFFSETS.into_iter().zip(SIZES.into_iter()).enumerate()
        {
            let offset = fixed_bytes::<OFFSET_LEN>(data, i * OFFSET_LEN)?;
            let size = fixed_bytes::<SIZE_LEN>(data, expected_offset)?;
            if !is_eq(offset, expected_offset) || !is_eq(size, expected_size) {
                return None;
            }
        }

        // Extract Data
        let pub_key: Bytes48 = fixed_bytes::<PUB_KEY_SIZE>(data, PUB_KEY_OFFSET + SIZE_LEN)?;
        let withdrawal_credentials: Bytes32 = fixed_bytes::<WITHDRAWAL_CREDENTIALS_SIZE>(
            data,
            WITHDRAWAL_CREDENTIALS_OFFSET + SIZE_LEN,
        )?;
        let amount: u64 =
            u64::from_le_bytes(fixed_bytes::<AMOUNT_SIZE>(data, AMOUNT_OFFSET + SIZE_LEN)?);
        let signature: Bytes96 = fixed_bytes::<SIGNATURE_SIZE>(data, SIGNATURE_OFFSET + SIZE_LEN)?;
        let index: u64 =
            u64::from_le_bytes(fixed_bytes::<INDEX_SIZE>(data, INDEX_OFFSET + SIZE_LEN)?);

        Some(Deposit {
            pub_key,
            withdrawal_credentials,
            amount,
            signature,
            index,
        })
    }

    pub fn to_summarized_byte_array(&self) -> [u8; 192] {
        let mut buffer = [0u8; 192];
        // pub_key + withdrawal_credentials + amount + signature + index
        let mut p = 0;
        buffer[p..48].clone_from_slice(&self.pub_key);
        p += 48;
        buffer[p..p + 32].clone_from_slice(&self.withdrawal_credentials);
        p += 32;
        buffer[p..p + 8].clone_from_slice(&self.amount.to_le_bytes());
        p += 8;
        buffer[p..p + 96].clone_from_slice(&self.signature);
        p += 96;
        buffer[p..p + 8].clone_from_slice(&self.index.to_le_bytes());

        buffer
    }
}

fn fixed_bytes<const N: usize>(data: &[u8], offset: usize) -> Option<[u8; N]> {
    data.get(offset..offset + N)?.try_into().ok()
}

// See https://github.com/ethereum/EIPs/blob/2a6b6965e64787815f7fffb9a4c27660d9683846/EIPS/eip-7685.md?plain=1#L62.
pub fn compute_requests_hash(requests: &[EncodedRequests]) -> H256 {
    let mut hasher = Sha256::new();
    for request in requests {
        let request_bytes = request.0.as_ref();
        if request_bytes.len() > 1 {
            hasher.update(Sha256::digest(request_bytes));
        }
    }
    H256::from_slice(&hasher.finalize())
}
