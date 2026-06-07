//! Order-construction requests (swap, limit, cancel).
//!
//! Builds Minswap-V2 order datum CBOR + payment instructions. The library
//! deals only in raw integers (e.g. `min_receive`); human-readable price /
//! token decimals are the caller's responsibility.

pub mod types;
pub mod swap;
pub mod cancel;
pub mod update;

pub use cancel::CancelSwapRequest;
pub use swap::SwapRequest;
pub use update::UpdateSwapRequest;
pub use types::{
    AddressType, AssetAmount, OrderKind, PayToAddress, PlutusScript, PlutusVersion, SpendUtxo,
    SwapFee, SwapParams, UtxoRef,
};
