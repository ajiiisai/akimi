#[derive(
    Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, serde::Serialize, serde::Deserialize,
)]
#[repr(transparent)]
pub struct NodeId(pub u32);

impl NodeId {
    pub const ROOT: Self = Self(0);

    pub fn index(self) -> usize {
        self.0 as usize
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NameRef {
    pub offset: u32,
    pub len: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[repr(u8)]
pub enum NodeKind {
    File,
    Directory,
    Symlink,
    Other,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Node {
    pub parent: NodeId,
    pub inode: u64,
    pub name: NameRef,
    pub kind: NodeKind,
    pub logical_size: u64,
    pub allocated_size: u64,
    pub links: u32,
    /// Modification time as a Unix timestamp (seconds). 0 when unknown.
    pub mtime: i64,
}
