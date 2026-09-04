mod aggregate;
mod arena;
mod node;
mod rank;

pub use aggregate::{aggregate, AggregateError, Totals};
pub use arena::{ArenaError, NameArena, NodeArena};
pub use node::{NameRef, Node, NodeId, NodeKind};
pub use rank::{rank_largest, RankFilter, RankedNode};

#[derive(Debug)]
pub struct ScanResult {
    pub arena: NodeArena,
    pub totals: Vec<Totals>,
}

impl ScanResult {
    pub fn new(arena: NodeArena) -> Result<Self, AggregateError> {
        let totals = aggregate(arena.nodes())?;
        Ok(Self { arena, totals })
    }
}
