// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
use std::collections::HashMap;
use std::sync::Arc;

use self::internal::Node;
use crate::pool_job::QueuedJob;

mod internal;

/// A FIFO queue that can remove a known entry without scanning its contents.
///
/// Callers serialize access with the owning pool's queue mutex. The queue
/// stores entries in reusable indexed slots and maintains a pointer-identity
/// index for ticket cancellation. Removed slots are immediately recycled, so
/// cancelled entries do not remain as tombstones.
pub(super) struct RemovableJobQueue {
    /// Indexed storage for live queue nodes and reusable empty slots.
    nodes: Vec<Node>,
    /// Indices of slots that can be reused by later insertions.
    free: Vec<usize>,
    /// Maps stable job pointer identities to their current node slots.
    indices: HashMap<usize, usize>,
    /// Index of the first live node, if any.
    head: Option<usize>,
    /// Index of the last live node, if any.
    tail: Option<usize>,
}

impl RemovableJobQueue {
    /// Creates an empty FIFO with no allocated node slots.
    pub(super) fn new() -> Self {
        Self {
            nodes: Vec::new(),
            free: Vec::new(),
            indices: HashMap::new(),
            head: None,
            tail: None,
        }
    }

    /// Appends an entry to the FIFO and records its stable allocation identity.
    ///
    /// # Parameters
    ///
    /// * `entry` - Accepted queue entry to append.
    ///
    /// The owning pool serializes this operation with its queue mutex.
    pub(super) fn push_back(&mut self, entry: Arc<QueuedJob>) {
        let identity = Arc::as_ptr(&entry) as usize;
        debug_assert!(!self.indices.contains_key(&identity));

        let index = self.free.pop().unwrap_or_else(|| {
            self.nodes.push(Node::default());
            self.nodes.len() - 1
        });
        let previous = self.tail;
        self.nodes[index] = Node {
            entry: Some(entry),
            previous,
            next: None,
        };
        if let Some(previous) = previous {
            self.nodes[previous].next = Some(index);
        } else {
            self.head = Some(index);
        }
        self.tail = Some(index);
        let replaced = self.indices.insert(identity, index);
        debug_assert!(replaced.is_none());
    }

    /// Returns the live slot for an entry, if the queue currently owns it.
    ///
    /// # Parameters
    ///
    /// * `entry` - Stable queue entry identity to look up.
    ///
    /// Pointer identity is stable while the supplied `Arc` is alive.
    pub(super) fn index_of(&self, entry: &Arc<QueuedJob>) -> Option<usize> {
        self.indices.get(&(Arc::as_ptr(entry) as usize)).copied()
    }

    /// Unlinks and returns the entry at `index`, recycling its storage slot.
    ///
    /// # Parameters
    ///
    /// * `index` - Current slot returned by [`Self::index_of`].
    ///
    /// The caller must pass an index returned for a currently linked entry.
    pub(super) fn remove(&mut self, index: usize) -> Arc<QueuedJob> {
        let node = &self.nodes[index];
        let previous = node.previous;
        let next = node.next;
        let entry = node.entry.as_ref().expect("linked queue node has an entry");
        let identity = Arc::as_ptr(entry) as usize;

        if let Some(previous) = previous {
            self.nodes[previous].next = next;
        } else {
            self.head = next;
        }
        if let Some(next) = next {
            self.nodes[next].previous = previous;
        } else {
            self.tail = previous;
        }

        let mut node = std::mem::take(&mut self.nodes[index]);
        let entry = node.entry.take().expect("linked queue node has an entry");
        let removed = self.indices.remove(&identity);
        debug_assert_eq!(removed, Some(index));
        self.free.push(index);
        entry
    }

    /// Removes and returns the oldest entry, preserving FIFO claim order.
    pub(super) fn pop_front(&mut self) -> Option<Arc<QueuedJob>> {
        self.head.map(|index| self.remove(index))
    }

    /// Returns the number of entries currently linked in the FIFO.
    pub(super) fn len(&self) -> usize {
        self.indices.len()
    }

    /// Checks that links and pointer-identity membership agree.
    #[cfg(test)]
    fn assert_valid(&self) {
        let mut current = self.head;
        let mut previous = None;
        let mut seen = 0;
        while let Some(index) = current {
            let node = &self.nodes[index];
            assert_eq!(node.previous, previous);
            let entry = node.entry.as_ref().expect("linked node has entry");
            assert_eq!(self.index_of(entry), Some(index));
            previous = Some(index);
            current = node.next;
            seen += 1;
            assert!(seen <= self.nodes.len(), "queue link cycle");
        }
        assert_eq!(previous, self.tail);
        assert_eq!(seen, self.indices.len());
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::RemovableJobQueue;
    use crate::PoolJob;

    fn entry() -> Arc<crate::pool_job::QueuedJob> {
        Arc::new(crate::pool_job::QueuedJob::new(PoolJob::new(
            Box::new(|| {}),
            Box::new(|| {}),
        )))
    }

    #[test]
    fn test_removable_queue_handles_singleton_head_middle_tail_and_reuse() {
        let first = entry();
        let second = entry();
        let third = entry();
        let fourth = entry();
        let mut queue = RemovableJobQueue::new();

        queue.push_back(Arc::clone(&first));
        assert!(Arc::ptr_eq(&queue.remove(queue.index_of(&first).unwrap()), &first));
        assert!(queue.index_of(&first).is_none());
        queue.assert_valid();

        queue.push_back(Arc::clone(&first));
        queue.push_back(Arc::clone(&second));
        queue.push_back(Arc::clone(&third));
        assert!(Arc::ptr_eq(&queue.remove(queue.index_of(&second).unwrap()), &second));
        assert!(Arc::ptr_eq(&queue.remove(queue.index_of(&first).unwrap()), &first));
        queue.push_back(Arc::clone(&fourth));
        queue.assert_valid();

        assert!(Arc::ptr_eq(&queue.pop_front().unwrap(), &third));
        assert!(Arc::ptr_eq(&queue.pop_front().unwrap(), &fourth));
        assert!(queue.pop_front().is_none());
        queue.assert_valid();
    }

    #[test]
    fn test_removable_queue_preserves_fifo_while_removing_arbitrary_entries() {
        let entries: Vec<_> = (0..6).map(|_| entry()).collect();
        let mut queue = RemovableJobQueue::new();
        for entry in &entries {
            queue.push_back(Arc::clone(entry));
        }

        for index in [2, 5, 0] {
            let entry = &entries[index];
            let slot = queue.index_of(entry).expect("entry remains linked");
            assert!(Arc::ptr_eq(&queue.remove(slot), entry));
            queue.assert_valid();
        }

        for index in [1, 3, 4] {
            assert!(Arc::ptr_eq(&queue.pop_front().unwrap(), &entries[index]));
            queue.assert_valid();
        }
        assert_eq!(queue.len(), 0);
        assert!(queue.pop_front().is_none());
    }
}
