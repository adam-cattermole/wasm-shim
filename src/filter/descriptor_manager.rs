use std::hash::Hash;

#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct DescriptorKey {
    pub cluster: String,
    pub service: String,
}

impl DescriptorKey {
    pub fn new(cluster: String, service: String) -> Self {
        Self { cluster, service }
    }
}
