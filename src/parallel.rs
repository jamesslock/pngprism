//! Explicit, deterministic execution schedules for the v0.4 parallel path.

use std::num::NonZeroUsize;
use std::ops::Range;

use crate::Error;

/// Hard ceiling on caller-requested stage workers.
pub const MAX_THREADS: usize = 256;

/// Deterministic histogram reduction order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeOrder {
    /// Pair adjacent states repeatedly until one root remains.
    Balanced,
    /// Fold shards from the lowest index to the highest.
    Forward,
    /// Fold shards from the highest index to the lowest.
    Reverse,
    /// Deterministically permute shard indices, then fold them.
    Shuffled(u64),
}

/// Opt-in execution schedule. One thread is the behavioral oracle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Parallelism {
    threads: NonZeroUsize,
    merge_order: MergeOrder,
}

impl Parallelism {
    /// The unchanged one-thread execution path.
    pub const SEQUENTIAL: Self = Self {
        threads: NonZeroUsize::MIN,
        merge_order: MergeOrder::Balanced,
    };

    /// Construct a balanced schedule with an explicit positive thread count.
    ///
    /// # Errors
    ///
    /// Returns a usage error when `threads` is zero or exceeds
    /// [`MAX_THREADS`].
    pub fn new(threads: usize) -> Result<Self, Error> {
        let threads = NonZeroUsize::new(threads).filter(|value| value.get() <= MAX_THREADS);
        let threads = threads.ok_or_else(|| {
            Error::usage(format!(
                "usage_error: --threads must be an integer in 1..={MAX_THREADS}"
            ))
        })?;
        Ok(Self {
            threads,
            merge_order: MergeOrder::Balanced,
        })
    }

    /// Select an explicit deterministic histogram merge order.
    #[must_use]
    pub const fn with_merge_order(mut self, merge_order: MergeOrder) -> Self {
        self.merge_order = merge_order;
        self
    }

    /// Requested thread count before per-stage work-size capping.
    #[must_use]
    pub const fn threads(self) -> usize {
        self.threads.get()
    }

    /// Deterministic histogram reduction order.
    #[must_use]
    pub const fn merge_order(self) -> MergeOrder {
        self.merge_order
    }

    pub(crate) const fn is_parallel(self) -> bool {
        self.threads.get() > 1
    }
}

pub(crate) fn shard_ranges(len: usize, requested: usize) -> Vec<Range<usize>> {
    if len == 0 {
        return Vec::new();
    }
    let count = requested.clamp(1, len);
    let base = len / count;
    let remainder = len % count;
    let mut ranges = Vec::with_capacity(count);
    let mut start = 0usize;
    for index in 0..count {
        let width = base + usize::from(index < remainder);
        let end = start + width;
        ranges.push(start..end);
        start = end;
    }
    ranges
}

pub(crate) fn map_ranges<T, F>(
    len: usize,
    parallelism: Parallelism,
    worker: F,
) -> Result<Vec<T>, Error>
where
    T: Send,
    F: Fn(Range<usize>) -> Result<T, Error> + Sync,
{
    let ranges = shard_ranges(len, parallelism.threads());
    if ranges.len() <= 1 {
        return ranges.into_iter().map(worker).collect();
    }
    std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(ranges.len());
        let worker = &worker;
        for range in ranges {
            match std::thread::Builder::new().spawn_scoped(scope, move || worker(range)) {
                Ok(handle) => handles.push(handle),
                Err(error) => {
                    for handle in handles {
                        let _ = handle.join();
                    }
                    return Err(Error::internal(format!(
                        "internal: cannot spawn parallel worker: {error}"
                    )));
                }
            }
        }
        // Join every worker before propagating any worker error. Otherwise an
        // early `?` could leave a later panicking child to re-panic the scope.
        let joined: Vec<_> = handles.into_iter().map(|handle| handle.join()).collect();
        let mut outputs = Vec::with_capacity(joined.len());
        for result in joined {
            let output = result
                .map_err(|_| Error::internal("internal: parallel worker panicked".to_string()))??;
            outputs.push(output);
        }
        Ok(outputs)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shard_ranges_are_complete_disjoint_and_balanced() {
        assert_eq!(shard_ranges(0, 7), Vec::<Range<usize>>::new());
        assert_eq!(shard_ranges(3, 7), vec![0..1, 1..2, 2..3]);
        assert_eq!(shard_ranges(10, 3), vec![0..4, 4..7, 7..10]);
    }

    #[test]
    fn zero_threads_is_rejected() {
        let error = Parallelism::new(0).unwrap_err();
        assert_eq!(
            error.to_string(),
            "usage_error: --threads must be an integer in 1..=256"
        );
        assert!(Parallelism::new(MAX_THREADS + 1).is_err());
    }
}
