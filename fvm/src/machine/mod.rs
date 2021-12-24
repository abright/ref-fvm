use blockstore::Blockstore;
use cid::Cid;
use wasmtime::{Engine, Module};

use fvm_shared::address::Address;
use fvm_shared::clock::ChainEpoch;
use fvm_shared::econ::TokenAmount;
use fvm_shared::error::ExitCode;
use fvm_shared::version::NetworkVersion;
use fvm_shared::ActorID;

use crate::externs::Externs;
use crate::gas::PriceList;
use crate::kernel::Result;
use crate::state_tree::{ActorState, StateTree};
use crate::Config;

mod default;
pub use default::DefaultMachine;

mod boxed;

pub const REWARD_ACTOR_ADDR: Address = Address::new_id(2);
/// Distinguished AccountActor that is the destination of all burnt funds.
pub const BURNT_FUNDS_ACTOR_ADDR: Address = Address::new_id(99);

pub trait Machine: 'static {
    type Blockstore: Blockstore;
    type Externs: Externs;

    /// Returns the underlying wasmtime engine. Cloning it will simply create a new handle
    /// with a static lifetime.
    fn engine(&self) -> &Engine;

    /// Returns the FVM's configuration.
    fn config(&self) -> &Config;

    /// Returns a reference to the machine's blockstore.
    fn blockstore(&self) -> &Self::Blockstore;

    /// Returns a reference to the machine context: static information about the current execution
    /// context.
    fn context(&self) -> &MachineContext;

    /// Returns a reference to all "node" supplied APIs.
    fn externs(&self) -> &Self::Externs;

    /// Returns an immutable reference to the state tree.
    fn state_tree(&self) -> &StateTree<Self::Blockstore>;

    /// Returns a mutable reference to the state tree.
    fn state_tree_mut(&mut self) -> &mut StateTree<Self::Blockstore>;

    /// Creates an uninitialized actor.
    // TODO: Remove
    fn create_actor(&mut self, addr: &Address, act: ActorState) -> Result<ActorID>;

    /// Loads a wasm module by CID.
    fn load_module(&self, code: &Cid) -> Result<Module>;

    /// Transfers tokens from one actor to another.
    ///
    /// If either the receiver or the sender do not exist, this method fails with a FATAL error.
    /// Otherwise, if the amounts are invalid, etc., it fails with a syscall error.
    fn transfer(&mut self, from: ActorID, to: ActorID, value: &TokenAmount) -> Result<()>;
}

/// An error included in a message's backtrace on failure.
#[derive(Clone, Debug)]
pub struct CallError {
    /// The source of the error or 0 for a syscall error.
    pub source: ActorID,
    /// The error code.
    pub code: ExitCode,
    /// The error message.
    pub message: String,
}

/// Execution context supplied to the machine.
#[derive(Clone, Debug)]
pub struct MachineContext {
    /// The epoch at which the Machine runs.
    pub epoch: ChainEpoch,
    /// The base fee that's in effect when the Machine runs.
    pub base_fee: TokenAmount,
    /// The initial state root on which this block is based.
    pub initial_state_root: Cid,
    /// The price list.
    pub price_list: PriceList,
    /// The network version at epoch
    pub network_version: NetworkVersion,
}
