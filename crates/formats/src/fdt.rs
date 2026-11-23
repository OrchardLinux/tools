//! Flattened device trees, read into nodes and written back.
//!
//! Enough to set a property and keep everything else: every node, property
//! and memory reservation of the input is in the output, in its order. The
//! output is laid out as orchard's `image/cascadia.py` writes a tree, so a
//! tree changed here is the same bytes as one changed there.
use crate::{Error, Result};

const MAGIC: u32 = 0xd00d_feed;
const HEADER: usize = 40;
const BEGIN_NODE: u32 = 1;
const END_NODE: u32 = 2;
const PROPERTY: u32 = 3;
const NOP: u32 = 4;
const END: u32 = 9;
/// Trees nested deeper than this are refused.
const MAX_DEPTH: usize = 128;

/// One node: its properties and its children, each in the tree's order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Node {
    pub name: String,
    pub properties: Vec<(String, Vec<u8>)>,
    pub children: Vec<Node>,
}

impl Node {
    /// Sets a property: in its place when the node has it, else behind the
    /// node's other properties.
    pub fn set(&mut self, name: &str, value: Vec<u8>) {
        match self.properties.iter_mut().find(|(n, _)| n == name) {
            Some((_, old)) => *old = value,
            None => self.properties.push((name.to_owned(), value)),
        }
    }
}

/// A device tree: the root node, the memory reservations and the boot CPU.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tree {
    pub root: Node,
    pub reservations: Vec<(u64, u64)>,
    pub boot_cpu: u32,
}

fn word(data: &[u8], at: usize) -> Result<u32> {
    let bytes = data.get(at..at + 4).ok_or(Error("truncated device tree"))?;
    Ok(u32::from_be_bytes(bytes.try_into().unwrap()))
}

/// The NUL-terminated ASCII string at `at`, which has to end before `end`.
fn string(data: &[u8], at: usize, end: usize) -> Result<&str> {
    let rest = data
        .get(at..end)
        .ok_or(Error("device tree string outside its block"))?;
    let length = rest
        .iter()
        .position(|&b| b == 0)
        .ok_or(Error("unterminated device tree string"))?;
    let text = &rest[..length];
    if !text.is_ascii() {
        return Err(Error("device tree names must be ASCII"));
    }
    Ok(std::str::from_utf8(text).expect("ASCII is UTF-8"))
}

fn aligned(value: usize) -> usize {
    value.next_multiple_of(4)
}

impl Tree {
    /// Reads a version-17 tree whose total size is the size of `data`.
    pub fn read(data: &[u8]) -> Result<Tree> {
        if data.len() < HEADER {
            return Err(Error("truncated device tree"));
        }
        let field = |index: usize| word(data, index * 4).map(|w| w as usize);
        let (total, tree_at, strings_at, reserve_at) = (field(1)?, field(2)?, field(3)?, field(4)?);
        let (version, last, strings_size, tree_size) = (field(5)?, field(6)?, field(8)?, field(9)?);
        if field(0)? != MAGIC as usize || total != data.len() || version < 17 || last > 17 {
            return Err(Error("unsupported device tree header"));
        }
        let inside = |at: usize, size: usize| {
            at >= HEADER && at.checked_add(size).is_some_and(|end| end <= total)
        };
        if tree_at % 4 != 0
            || reserve_at % 8 != 0
            || tree_size == 0
            || !inside(tree_at, tree_size)
            || !inside(strings_at, strings_size)
            || !inside(reserve_at, 16)
        {
            return Err(Error("invalid device tree block bounds"));
        }
        let blocks = [
            (tree_at, tree_at + tree_size),
            (strings_at, strings_at + strings_size),
        ];
        if blocks[0].0.max(blocks[1].0) < blocks[0].1.min(blocks[1].1) {
            return Err(Error("device tree blocks overlap"));
        }
        let mut reservations = Vec::new();
        let mut at = reserve_at;
        loop {
            let outside = |&(start, end): &(usize, usize)| at + 16 <= start || at >= end;
            if at + 16 > total || !blocks.iter().all(outside) {
                return Err(Error("device tree reservation map runs into another block"));
            }
            let quad =
                |at: usize| Ok((u64::from(word(data, at)?) << 32) | u64::from(word(data, at + 4)?));
            let (address, length): (u64, u64) = (quad(at)?, quad(at + 8)?);
            at += 16;
            if (address, length) == (0, 0) {
                break;
            }
            if length == 0 || address.checked_add(length).is_none() {
                return Err(Error("invalid device tree reservation"));
            }
            reservations.push((address, length));
        }

        let end = tree_at + tree_size;
        let strings_end = strings_at + strings_size;
        let mut at = tree_at;
        let mut open: Vec<Node> = Vec::new();
        let mut root = None;
        loop {
            if at + 4 > end {
                return Err(Error("the device tree lacks its end token"));
            }
            let token = word(data, at)?;
            at += 4;
            match token {
                BEGIN_NODE => {
                    let name = string(data, at, end)?;
                    at = aligned(at + name.len() + 1);
                    let nested = open.len() >= MAX_DEPTH || name.contains('/');
                    // Only the root has no name, and there is one root.
                    if nested || at > end || open.is_empty() != name.is_empty() || root.is_some() {
                        return Err(Error("invalid device tree node"));
                    }
                    if open
                        .last()
                        .is_some_and(|p| p.children.iter().any(|c| c.name == name))
                    {
                        return Err(Error("repeated device tree child node"));
                    }
                    open.push(Node {
                        name: name.to_owned(),
                        ..Node::default()
                    });
                }
                END_NODE => {
                    let node = open.pop().ok_or(Error("unbalanced device tree end-node"))?;
                    match open.last_mut() {
                        Some(parent) => parent.children.push(node),
                        None => root = Some(node),
                    }
                }
                PROPERTY => {
                    let node = open.last_mut().filter(|n| n.children.is_empty());
                    let node = node.ok_or(Error("invalid device tree property position"))?;
                    let (length, name_at) =
                        (word(data, at)? as usize, word(data, at + 4)? as usize);
                    at += 8;
                    let name = string(data, strings_at.saturating_add(name_at), strings_end)?;
                    let value = at
                        .checked_add(length)
                        .filter(|&to| to <= end)
                        .and_then(|to| data.get(at..to))
                        .ok_or(Error("a device tree property extends past its block"))?;
                    if name.is_empty() || node.properties.iter().any(|(n, _)| n == name) {
                        return Err(Error("empty or repeated device tree property"));
                    }
                    node.properties.push((name.to_owned(), value.to_vec()));
                    at = aligned(at + length);
                }
                NOP => {}
                END if open.is_empty() && at == end => break,
                _ => return Err(Error("unknown or misplaced device tree token")),
            }
        }
        Ok(Tree {
            root: root.ok_or(Error("the device tree has no root node"))?,
            reservations,
            boot_cpu: word(data, 28)?,
        })
    }

    /// Writes the tree: header, reservations, nodes, then the property
    /// names, each once in the order it is first used.
    pub fn write(&self) -> Vec<u8> {
        fn emit(
            node: &Node,
            tree: &mut Vec<u8>,
            names: &mut Vec<u8>,
            known: &mut Vec<(String, u32)>,
        ) {
            tree.extend(BEGIN_NODE.to_be_bytes());
            tree.extend(node.name.as_bytes());
            tree.push(0);
            tree.resize(aligned(tree.len()), 0);
            for (name, value) in &node.properties {
                let at = match known.iter().find(|(n, _)| n == name) {
                    Some((_, at)) => *at,
                    None => {
                        let at = names.len() as u32;
                        names.extend(name.as_bytes());
                        names.push(0);
                        known.push((name.clone(), at));
                        at
                    }
                };
                tree.extend(PROPERTY.to_be_bytes());
                tree.extend((value.len() as u32).to_be_bytes());
                tree.extend(at.to_be_bytes());
                tree.extend(value);
                tree.resize(aligned(tree.len()), 0);
            }
            for child in &node.children {
                emit(child, tree, names, known);
            }
            tree.extend(END_NODE.to_be_bytes());
        }
        let (mut tree, mut names) = (Vec::new(), Vec::new());
        emit(&self.root, &mut tree, &mut names, &mut Vec::new());
        tree.extend(END.to_be_bytes());
        // A tree without properties keeps a one-byte names block.
        if names.is_empty() {
            names.push(0);
        }
        let tree_at = HEADER + (self.reservations.len() + 1) * 16;
        let strings_at = tree_at + tree.len();
        let header = [
            MAGIC,
            (strings_at + names.len()) as u32,
            tree_at as u32,
            strings_at as u32,
            HEADER as u32,
            17,
            16,
            self.boot_cpu,
            names.len() as u32,
            tree.len() as u32,
        ];
        let mut out: Vec<u8> = header.iter().flat_map(|w| w.to_be_bytes()).collect();
        for (address, length) in &self.reservations {
            out.extend(address.to_be_bytes());
            out.extend(length.to_be_bytes());
        }
        out.extend([0; 16]);
        out.extend(tree);
        out.extend(names);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Tree {
        let leaf = |name: &str, properties: &[(&str, &[u8])]| Node {
            name: name.into(),
            properties: properties
                .iter()
                .map(|(n, v)| ((*n).to_owned(), v.to_vec()))
                .collect(),
            children: Vec::new(),
        };
        Tree {
            root: Node {
                name: String::new(),
                properties: vec![("compatible".into(), b"example,board\0".to_vec())],
                children: vec![
                    leaf(
                        "chosen",
                        &[("bootargs", b"quiet\0"), ("reg", &[0, 0, 0, 1])],
                    ),
                    // The same property name again shares its place in the names.
                    leaf(
                        "memory@80000000",
                        &[("reg", &[0x80, 0, 0, 0, 0x20, 0, 0, 0])],
                    ),
                ],
            },
            reservations: vec![(0x8000_0000, 0x1000)],
            boot_cpu: 2,
        }
    }

    #[test]
    fn a_tree_reads_back_as_it_was_written() {
        let tree = sample();
        let data = tree.write();
        assert_eq!(word(&data, 4).unwrap() as usize, data.len());
        assert_eq!(Tree::read(&data).unwrap(), tree);
        // `reg` is in the names once.
        let names = &data[word(&data, 12).unwrap() as usize..];
        assert_eq!(names, b"compatible\0bootargs\0reg\0");
        // Written again from what was read: the same bytes.
        assert_eq!(Tree::read(&data).unwrap().write(), data);
    }

    #[test]
    fn a_property_is_set_in_place_or_added_behind_the_others() {
        let mut tree = sample();
        tree.root.set("serial-number", b"F4K\0".to_vec());
        tree.root.set("compatible", b"other\0".to_vec());
        let names: Vec<&str> = tree
            .root
            .properties
            .iter()
            .map(|(n, _)| n.as_str())
            .collect();
        assert_eq!(names, ["compatible", "serial-number"]);
        assert_eq!(tree.root.properties[0].1, b"other\0");
        let read = Tree::read(&tree.write()).unwrap();
        assert_eq!(read, tree);
        // The children and the reservation came along.
        assert_eq!(read.root.children.len(), 2);
        assert_eq!(read.reservations, [(0x8000_0000, 0x1000)]);
    }

    #[test]
    fn damaged_trees_are_refused() {
        let data = sample().write();
        assert!(Tree::read(&data[..data.len() - 1]).is_err());
        assert!(Tree::read(&data[..20]).is_err());
        let mut magic = data.clone();
        magic[0] = 0;
        assert_eq!(
            Tree::read(&magic).unwrap_err().0,
            "unsupported device tree header"
        );
        // The end token replaced by something unknown.
        let tree_at = word(&data, 8).unwrap() as usize;
        let tree_size = word(&data, 36).unwrap() as usize;
        let mut token = data.clone();
        token[tree_at + tree_size - 1] = 7;
        assert!(Tree::read(&token).is_err());
        // A property name that points past the names.
        let mut name = data.clone();
        name[tree_at + 16..tree_at + 20].copy_from_slice(&0xffffu32.to_be_bytes());
        assert!(Tree::read(&name).is_err());
    }
}
