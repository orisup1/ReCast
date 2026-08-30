//! Allocation-free lookup over sorted dictionary and frequency blobs.

#[derive(Clone, Copy)]
pub struct Dict {
    pub(super) blob: &'static str,
}

#[derive(Clone, Copy)]
pub struct Freq {
    pub(super) blob: &'static str,
}

impl Dict {
    pub const fn new(blob: &'static str) -> Self {
        Self { blob }
    }

    pub fn contains(self, word: &str) -> bool {
        lookup(self.blob.as_bytes(), word.as_bytes()).is_some()
    }
}

impl Freq {
    pub const fn new(blob: &'static str) -> Self {
        Self { blob }
    }

    pub fn rank(self, word: &str) -> Option<u32> {
        let line = lookup(self.blob.as_bytes(), word.as_bytes())?;
        let tab = line.iter().position(|&byte| byte == b'\t')?;
        parse_rank(&line[tab + 1..])
    }

    pub fn for_each_with_prefix(self, prefix: &str, mut visit: impl FnMut(&str, u32)) {
        let blob = self.blob.as_bytes();
        let mut pos = lower_bound(blob, prefix.as_bytes());
        while pos < blob.len() {
            let end = pos
                + blob[pos..]
                    .iter()
                    .position(|&byte| byte == b'\n')
                    .unwrap_or(blob.len() - pos);
            let line = &blob[pos..end];
            let key = key_of(line);
            if !key.starts_with(prefix.as_bytes()) {
                return;
            }
            if let (Ok(word), Some(rank)) = (
                std::str::from_utf8(key),
                line.get(key.len() + 1..).and_then(parse_rank),
            ) {
                visit(word, rank);
            }
            pos = end + 1;
        }
    }
}

fn key_of(line: &[u8]) -> &[u8] {
    match line.iter().position(|&byte| byte == b'\t') {
        Some(index) => &line[..index],
        None => line,
    }
}

fn parse_rank(bytes: &[u8]) -> Option<u32> {
    let mut rank = 0u32;
    for &byte in bytes {
        rank = rank
            .checked_mul(10)?
            .checked_add(byte.checked_sub(b'0')? as u32)?;
    }
    Some(rank)
}

fn lookup<'a>(blob: &'a [u8], needle: &[u8]) -> Option<&'a [u8]> {
    let (mut low, mut high) = (0usize, blob.len());
    while low < high {
        let middle = low + (high - low) / 2;
        let start = match blob[low..middle].iter().rposition(|&byte| byte == b'\n') {
            Some(index) => low + index + 1,
            None => low,
        };
        let end = start
            + blob[start..]
                .iter()
                .position(|&byte| byte == b'\n')
                .unwrap_or(blob.len() - start);
        let line = &blob[start..end];
        match key_of(line).cmp(needle) {
            std::cmp::Ordering::Less => low = end + 1,
            std::cmp::Ordering::Greater => high = start,
            std::cmp::Ordering::Equal => return Some(line),
        }
    }
    None
}

fn lower_bound(blob: &[u8], needle: &[u8]) -> usize {
    let (mut low, mut high) = (0usize, blob.len());
    while low < high {
        let middle = low + (high - low) / 2;
        let start = match blob[low..middle].iter().rposition(|&byte| byte == b'\n') {
            Some(index) => low + index + 1,
            None => low,
        };
        let end = start
            + blob[start..]
                .iter()
                .position(|&byte| byte == b'\n')
                .unwrap_or(blob.len() - start);
        if key_of(&blob[start..end]) < needle {
            low = end + 1;
        } else {
            high = start;
        }
    }
    low.min(blob.len())
}

pub const fn en_dict() -> Dict {
    Dict::new(include_str!(concat!(env!("OUT_DIR"), "/en_dict.blob")))
}

pub const fn he_dict() -> Dict {
    Dict::new(include_str!(concat!(env!("OUT_DIR"), "/he_dict.blob")))
}

pub const fn en_freq() -> Freq {
    Freq::new(include_str!(concat!(env!("OUT_DIR"), "/en_freq.blob")))
}

pub const fn he_freq() -> Freq {
    Freq::new(include_str!(concat!(env!("OUT_DIR"), "/he_freq.blob")))
}
