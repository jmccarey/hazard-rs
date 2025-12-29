use std::collections::HashSet;

pub struct RetiredNode {
    ptr: *mut u8,
    deleter: fn(*mut u8),
}

impl RetiredNode {
    pub fn new(ptr: *mut u8, deleter: fn(*mut u8)) -> Self {
        Self { ptr, deleter }
    }

    pub fn address(&self) -> usize {
        self.ptr as usize
    }

    pub fn delete(self) {
        (self.deleter)(self.ptr);
    }
}

pub struct RetiredList {
    nodes: Vec<RetiredNode>,
}

impl RetiredList {
    pub fn new() -> Self {
        Self { nodes: Vec::new() }
    }

    pub fn push(&mut self, ptr: *mut u8, deleter: fn(*mut u8)) {
        self.nodes.push(RetiredNode::new(ptr, deleter));
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn reclaim(&mut self, protected: &HashSet<usize>) {
        let mut i = 0;
        while i < self.nodes.len() {
            if !protected.contains(&self.nodes[i].address()) {
                let node = self.nodes.swap_remove(i);
                node.delete();
            } else {
                i += 1;
            }
        }
    }

    pub fn force_reclaim_all(&mut self) {
        for node in self.nodes.drain(..) {
            node.delete();
        }
    }
}

impl Default for RetiredList {
    fn default() -> Self {
        Self::new()
    }
}

unsafe impl Send for RetiredList {}

