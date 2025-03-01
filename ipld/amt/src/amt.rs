// Copyright 2019-2022 ChainSafe Systems
// SPDX-License-Identifier: Apache-2.0, MIT

use anyhow::anyhow;
use cid::multihash::Code;
use cid::Cid;
use fvm_ipld_blockstore::Blockstore;
use fvm_ipld_encoding::de::DeserializeOwned;
use fvm_ipld_encoding::ser::Serialize;
use fvm_ipld_encoding::CborStore;
use itertools::sorted;

use super::ValueMut;
use crate::node::{CollapsedNode, Link};
use crate::root::version::{Version as AmtVersion, V0, V3};
use crate::root::RootImpl;
use crate::{
    init_sized_vec, nodes_for_height, Error, Node, DEFAULT_BIT_WIDTH, MAX_HEIGHT, MAX_INDEX,
};

#[derive(Debug)]
#[doc(hidden)]
pub struct AmtImpl<V, BS, Ver> {
    root: RootImpl<V, Ver>,
    block_store: BS,
}

/// Array Mapped Trie allows for the insertion and persistence of data, serializable to a CID.
///
/// Amt is not threadsafe and can't be shared between threads.
///
/// Usage:
/// ```
/// use fvm_ipld_amt::Amt;
///
/// let db = fvm_ipld_blockstore::MemoryBlockstore::default();
/// let mut amt = Amt::new(&db);
///
/// // Insert or remove any serializable values
/// amt.set(2, "foo".to_owned()).unwrap();
/// amt.set(1, "bar".to_owned()).unwrap();
/// amt.delete(2).unwrap();
/// assert_eq!(amt.count(), 1);
/// let bar: &String = amt.get(1).unwrap().unwrap();
///
/// // Generate cid by calling flush to remove cache
/// let cid = amt.flush().unwrap();
/// ```
pub type Amt<V, BS> = AmtImpl<V, BS, V3>;
/// Legacy amt V0
pub type Amtv0<V, BS> = AmtImpl<V, BS, V0>;

impl<V: PartialEq, BS: Blockstore, Ver: PartialEq> PartialEq for AmtImpl<V, BS, Ver> {
    fn eq(&self, other: &Self) -> bool {
        self.root == other.root
    }
}

impl<V, BS, Ver> AmtImpl<V, BS, Ver>
where
    V: DeserializeOwned + Serialize,
    BS: Blockstore,
    Ver: AmtVersion,
{
    /// Constructor for Root AMT node
    pub fn new(block_store: BS) -> Self {
        Self::new_with_bit_width(block_store, DEFAULT_BIT_WIDTH)
    }

    /// Construct new Amt with given bit width
    pub fn new_with_bit_width(block_store: BS, bit_width: u32) -> Self {
        Self {
            root: RootImpl::new_with_bit_width(bit_width),
            block_store,
        }
    }

    fn bit_width(&self) -> u32 {
        self.root.bit_width
    }

    /// Constructs an AMT with a blockstore and a Cid of the root of the AMT
    pub fn load(cid: &Cid, block_store: BS) -> Result<Self, Error> {
        // Load root bytes from database
        let root: RootImpl<V, Ver> = block_store
            .get_cbor(cid)?
            .ok_or_else(|| Error::CidNotFound(cid.to_string()))?;

        // Sanity check, this should never be possible.
        if root.height > MAX_HEIGHT {
            return Err(Error::MaxHeight(root.height, MAX_HEIGHT));
        }

        Ok(Self { root, block_store })
    }

    /// Gets the height of the `Amt`.
    pub fn height(&self) -> u32 {
        self.root.height
    }

    /// Gets count of elements added in the `Amt`.
    pub fn count(&self) -> u64 {
        self.root.count
    }

    /// Generates an AMT with block store and array of cbor marshallable objects and returns Cid
    pub fn new_from_iter(block_store: BS, vals: impl IntoIterator<Item = V>) -> Result<Cid, Error> {
        let mut t = Self::new(block_store);

        t.batch_set(vals)?;

        t.flush()
    }

    /// Get value at index of AMT
    pub fn get(&self, i: u64) -> Result<Option<&V>, Error> {
        if i > MAX_INDEX {
            return Err(Error::OutOfRange(i));
        }

        if i >= nodes_for_height(self.bit_width(), self.height() + 1) {
            return Ok(None);
        }

        self.root
            .node
            .get(&self.block_store, self.height(), self.bit_width(), i)
    }

    /// Set value at index
    pub fn set(&mut self, i: u64, val: V) -> Result<(), Error> {
        if i > MAX_INDEX {
            return Err(Error::OutOfRange(i));
        }

        while i >= nodes_for_height(self.bit_width(), self.height() + 1) {
            // node at index exists
            if !self.root.node.is_empty() {
                // Parent node for expansion
                let mut new_links: Vec<Option<Link<V>>> = init_sized_vec(self.root.bit_width);

                // Take root node to be moved down
                let node = std::mem::replace(&mut self.root.node, Node::empty());

                // Set link to child node being expanded
                new_links[0] = Some(Link::Dirty(Box::new(node)));

                self.root.node = Node::Link { links: new_links };
            } else {
                // If first expansion is before a value inserted, convert base node to Link
                self.root.node = Node::Link {
                    links: init_sized_vec(self.bit_width()),
                };
            }
            // Incrememnt height after each iteration
            self.root.height += 1;
        }

        if self
            .root
            .node
            .set(&self.block_store, self.height(), self.bit_width(), i, val)?
            .is_none()
        {
            self.root.count += 1;
        }

        Ok(())
    }

    /// Batch set (naive for now)
    // TODO Implement more efficient batch set to not have to traverse tree and keep cache for each
    pub fn batch_set(&mut self, vals: impl IntoIterator<Item = V>) -> Result<(), Error> {
        for (i, val) in (0u64..).zip(vals) {
            self.set(i, val)?;
        }

        Ok(())
    }

    /// Delete item from AMT at index
    pub fn delete(&mut self, i: u64) -> Result<Option<V>, Error> {
        if i > MAX_INDEX {
            return Err(Error::OutOfRange(i));
        }

        if i >= nodes_for_height(self.bit_width(), self.height() + 1) {
            // Index was out of range of current AMT
            return Ok(None);
        }

        // Delete node from AMT
        let deleted =
            self.root
                .node
                .delete(&self.block_store, self.height(), self.bit_width(), i)?;

        if deleted.is_none() {
            return Ok(None);
        }

        self.root.count -= 1;

        if self.root.node.is_empty() {
            // Last link was removed, replace root with a leaf node and reset height.
            self.root.node = Node::Leaf {
                vals: init_sized_vec(self.root.bit_width),
            };
            self.root.height = 0;
        } else {
            // Handle collapsing node when the root is a link node with only one link,
            // sub node can be moved up into the root.
            while self.root.node.can_collapse() && self.height() > 0 {
                let sub_node: Node<V> = match &mut self.root.node {
                    Node::Link { links, .. } => match &mut links[0] {
                        Some(Link::Dirty(node)) => {
                            *std::mem::replace(node, Box::new(Node::empty()))
                        }
                        Some(Link::Cid { cid, cache }) => {
                            let cache_node = std::mem::take(cache);
                            if let Some(sn) = cache_node.into_inner() {
                                *sn
                            } else {
                                // Only retrieve sub node if not found in cache
                                self.block_store
                                    .get_cbor::<CollapsedNode<V>>(cid)?
                                    .ok_or_else(|| Error::CidNotFound(cid.to_string()))?
                                    .expand(self.root.bit_width)?
                            }
                        }
                        _ => unreachable!("First index checked to be Some in `can_collapse`"),
                    },
                    Node::Leaf { .. } => unreachable!("Non zero height cannot be a leaf node"),
                };

                self.root.node = sub_node;
                self.root.height -= 1;
            }
        }

        Ok(deleted)
    }

    /// Deletes multiple items from AMT
    /// If `strict` is true, all indices are expected to be present, and this will
    /// return an error if one is not found.
    ///
    /// Returns true if items were deleted.
    pub fn batch_delete(
        &mut self,
        iter: impl IntoIterator<Item = u64>,
        strict: bool,
    ) -> Result<bool, Error> {
        // TODO: optimize this
        let mut modified = false;

        // Iterate sorted indices. Sorted to safely optimize later.
        for i in sorted(iter) {
            let found = self.delete(i)?.is_some();
            if strict && !found {
                return Err(anyhow!("no such index {} in Amt for batch delete", i).into());
            }
            modified |= found;
        }
        Ok(modified)
    }

    /// flush root and return Cid used as key in block store
    pub fn flush(&mut self) -> Result<Cid, Error> {
        self.root.node.flush(&self.block_store)?;
        Ok(self.block_store.put_cbor(&self.root, Code::Blake2b256)?)
    }

    /// Iterates over each value in the Amt and runs a function on the values.
    ///
    /// The index in the amt is a `u64` and the value is the generic parameter `V` as defined
    /// in the Amt.
    ///
    /// # Examples
    ///
    /// ```
    /// use fvm_ipld_amt::Amt;
    ///
    /// let store = fvm_ipld_blockstore::MemoryBlockstore::default();
    ///
    /// let mut map: Amt<String, _> = Amt::new(&store);
    /// map.set(1, "One".to_owned()).unwrap();
    /// map.set(4, "Four".to_owned()).unwrap();
    ///
    /// let mut values: Vec<(u64, String)> = Vec::new();
    /// map.for_each(|i, v| {
    ///    values.push((i, v.clone()));
    ///    Ok(())
    /// }).unwrap();
    /// assert_eq!(&values, &[(1, "One".to_owned()), (4, "Four".to_owned())]);
    /// ```
    #[inline]
    pub fn for_each<F>(&self, mut f: F) -> Result<(), Error>
    where
        F: FnMut(u64, &V) -> anyhow::Result<()>,
    {
        self.for_each_while(|i, x| {
            f(i, x)?;
            Ok(true)
        })
    }

    /// Iterates over each value in the Amt and runs a function on the values, for as long as that
    /// function keeps returning `true`.
    pub fn for_each_while<F>(&self, mut f: F) -> Result<(), Error>
    where
        F: FnMut(u64, &V) -> anyhow::Result<bool>,
    {
        self.root
            .node
            .for_each_while(
                &self.block_store,
                self.height(),
                self.bit_width(),
                0,
                &mut f,
            )
            .map(|_| ())
    }

    /// Iterates over each value in the Amt and runs a function on the values that allows modifying
    /// each value.
    pub fn for_each_mut<F>(&mut self, mut f: F) -> Result<(), Error>
    where
        V: Clone,
        F: FnMut(u64, &mut ValueMut<'_, V>) -> anyhow::Result<()>,
    {
        self.for_each_while_mut(|i, x| {
            f(i, x)?;
            Ok(true)
        })
    }

    /// Iterates over each value in the Amt and runs a function on the values that allows modifying
    /// each value, for as long as that function keeps returning `true`.
    pub fn for_each_while_mut<F>(&mut self, mut f: F) -> Result<(), Error>
    where
        // TODO remove clone bound when go-interop doesn't require it.
        // (If needed without, this bound can be removed by duplicating function signatures)
        V: Clone,
        F: FnMut(u64, &mut ValueMut<'_, V>) -> anyhow::Result<bool>,
    {
        #[cfg(not(feature = "go-interop"))]
        {
            self.root
                .node
                .for_each_while_mut(
                    &self.block_store,
                    self.height(),
                    self.bit_width(),
                    0,
                    &mut f,
                )
                .map(|_| ())
        }

        // TODO remove requirement for this when/if changed in go-implementation
        // This is not 100% compatible, because the blockstore reads/writes are not in the same
        // order. If this is to be achieved, the for_each iteration would have to pause when
        // a mutation occurs, set, then continue where it left off. This is a much more extensive
        // change, and since it should not be feasibly triggered, it's left as this for now.
        #[cfg(feature = "go-interop")]
        {
            let mut mutated = Vec::new();

            self.root.node.for_each_while_mut(
                &self.block_store,
                self.height(),
                self.bit_width(),
                0,
                &mut |idx, value| {
                    let keep_going = f(idx, value)?;

                    if value.value_changed() {
                        // ! this is not ideal to clone and mark unchanged here, it is only done
                        // because the go-implementation mutates the Amt as they iterate through it,
                        // which we cannot do because it is memory unsafe (and I'm not certain we
                        // don't have side effects from doing this unsafely)
                        value.mark_unchanged();
                        mutated.push((idx, value.clone()));
                    }

                    Ok(keep_going)
                },
            )?;

            for (i, v) in mutated {
                self.set(i, v)?;
            }

            Ok(())
        }
    }
}
