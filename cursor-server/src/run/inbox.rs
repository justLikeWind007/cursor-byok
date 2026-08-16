use std::collections::BTreeMap;

#[derive(Debug)]
pub struct OrderedInbox<T> {
    next: i64,
    pending: BTreeMap<i64, T>,
}

impl<T> Default for OrderedInbox<T> {
    fn default() -> Self {
        Self {
            next: 0,
            pending: BTreeMap::new(),
        }
    }
}

impl<T> OrderedInbox<T> {
    pub fn starting_at(next: i64) -> Self {
        Self {
            next,
            pending: BTreeMap::new(),
        }
    }

    pub fn push(&mut self, seqno: i64, value: T) -> Vec<(i64, T)> {
        if seqno < self.next {
            return Vec::new();
        }
        self.pending.entry(seqno).or_insert(value);
        let mut ready = Vec::new();
        while let Some(value) = self.pending.remove(&self.next) {
            ready.push((self.next, value));
            self.next += 1;
        }
        ready
    }
}
