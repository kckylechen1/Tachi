use memcore::MemoryStore;

mod gc;

#[cfg(test)]
mod tests;

pub(crate) use self::gc::gc_expired_kanban_cards;

pub(super) const KANBAN_PATH_PREFIX: &str = "/kanban/";
pub(crate) const DEFAULT_KANBAN_GC_MAX_AGE_DAYS: u64 = 30;

#[cfg(test)]
pub(super) const KANBAN_DISPATCH_PATH_PREFIX: &str = "/kanban/tasks/";
