//! Allocation-free lookup over sorted dictionary and frequency blobs.

#[derive(Clone, Copy)]
pub struct Dict {
    pub(super) blob: &'static str,
    index: PrefixIndex,
}

#[derive(Clone, Copy)]
pub struct Freq {
    pub(super) blob: &'static str,
    index: PrefixIndex,
}

/// Small build-time indexes narrow searches to at most two opening letters.
/// Characters outside the indexed alphabet fall back to a wider search.
/// Test dictionaries can use the original full search without an index.
#[derive(Clone, Copy)]
struct PrefixIndex {
    ranges: &'static [u8],
    first: char,
    letters: usize,
}

impl PrefixIndex {
    const NONE: Self = Self {
        ranges: &[],
        first: 'a',
        letters: 0,
    };

    fn range(self, text: &str, blob_len: usize) -> std::ops::Range<usize> {
        let letter_index = |letter: char| {
            (letter as u32)
                .checked_sub(self.first as u32)
                .map(|index| index as usize)
                .filter(|&index| index < self.letters)
        };
        let mut chars = text.chars();
        let Some(row) = chars.next().and_then(letter_index) else {
            return 0..blob_len;
        };
        let column = chars.next().and_then(letter_index).map_or(0, |i| i + 1);
        let offset = (row * (self.letters + 1) + column) * 8;
        let start = u32::from_le_bytes(self.ranges[offset..offset + 4].try_into().unwrap());
        let end = u32::from_le_bytes(self.ranges[offset + 4..offset + 8].try_into().unwrap());
        start as usize..end as usize
    }
}

impl Dict {
    pub const fn new(blob: &'static str) -> Self {
        Self {
            blob,
            index: PrefixIndex::NONE,
        }
    }

    pub fn contains(self, word: &str) -> bool {
        let range = self.index.range(word, self.blob.len());
        lookup(&self.blob.as_bytes()[range], word.as_bytes()).is_some()
    }
}

impl Freq {
    #[cfg(test)]
    pub const fn new(blob: &'static str) -> Self {
        Self {
            blob,
            index: PrefixIndex::NONE,
        }
    }

    pub fn rank(self, word: &str) -> Option<u32> {
        let range = self.index.range(word, self.blob.len());
        let line = lookup(&self.blob.as_bytes()[range], word.as_bytes())?;
        let tab = line.iter().position(|&byte| byte == b'\t')?;
        parse_rank(&line[tab + 1..])
    }

    pub fn for_each_with_prefix(self, prefix: &str, mut visit: impl FnMut(&str, u32)) {
        let range = self.index.range(prefix, self.blob.len());
        let blob = &self.blob.as_bytes()[range];
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
    Dict {
        blob: include_str!(concat!(env!("OUT_DIR"), "/en_dict.blob")),
        index: PrefixIndex {
            ranges: include_bytes!(concat!(env!("OUT_DIR"), "/en_dict.prefix")),
            first: 'a',
            letters: 26,
        },
    }
}

pub const fn he_dict() -> Dict {
    Dict {
        blob: include_str!(concat!(env!("OUT_DIR"), "/he_dict.blob")),
        index: PrefixIndex {
            ranges: include_bytes!(concat!(env!("OUT_DIR"), "/he_dict.prefix")),
            first: 'א',
            letters: 27,
        },
    }
}

pub const fn en_freq() -> Freq {
    Freq {
        blob: include_str!(concat!(env!("OUT_DIR"), "/en_freq.blob")),
        index: PrefixIndex {
            ranges: include_bytes!(concat!(env!("OUT_DIR"), "/en_freq.prefix")),
            first: 'a',
            letters: 26,
        },
    }
}

pub const fn he_freq() -> Freq {
    Freq {
        blob: include_str!(concat!(env!("OUT_DIR"), "/he_freq.blob")),
        index: PrefixIndex {
            ranges: include_bytes!(concat!(env!("OUT_DIR"), "/he_freq.prefix")),
            first: 'א',
            letters: 27,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_indexes_preserve_every_embedded_entry() {
        for dict in [en_dict(), he_dict()] {
            for word in dict.blob.lines() {
                assert!(
                    dict.contains(word),
                    "missing indexed dictionary entry: {word}"
                );
            }
            let plain = Dict::new(dict.blob);
            for word in ["", "a", "z", "א", "ת", "dont", "don't", "שלום", "~", "🙂"] {
                assert_eq!(dict.contains(word), plain.contains(word), "{word}");
            }
            for word in dict.blob.lines().step_by(997) {
                let absent = format!("{word}\0");
                assert!(!dict.contains(&absent), "{absent:?}");
            }
        }
        for freq in [en_freq(), he_freq()] {
            for line in freq.blob.lines() {
                let (word, rank) = line.split_once('\t').unwrap();
                assert_eq!(freq.rank(word), Some(rank.parse().unwrap()), "{word}");
            }
        }
    }

    #[test]
    fn indexed_prefix_scans_match_full_blob_searches() {
        for freq in [en_freq(), he_freq()] {
            let plain = Freq::new(freq.blob);
            let collect = |freq: Freq, prefix: &str| {
                let mut found = Vec::new();
                freq.for_each_with_prefix(prefix, |word, rank| {
                    found.push((word.to_owned(), rank));
                });
                found
            };
            for prefix in ["", "a", "z", "א", "ת", "don't", "שלו", "~", "🙂"] {
                assert_eq!(collect(freq, prefix), collect(plain, prefix), "{prefix}");
            }
            let start = freq.index.first as u32;
            for first in start..start + freq.index.letters as u32 {
                let first = char::from_u32(first).unwrap();
                let prefix = first.to_string();
                assert_eq!(collect(freq, &prefix), collect(plain, &prefix), "{prefix}");
                for second in start..start + freq.index.letters as u32 {
                    let second = char::from_u32(second).unwrap();
                    let prefix = format!("{first}{second}");
                    assert_eq!(collect(freq, &prefix), collect(plain, &prefix), "{prefix}");
                }
            }
        }
    }
}
