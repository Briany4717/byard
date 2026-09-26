//! The package archive `byard publish` writes and `byard get` unpacks
//! (RFC-0008 D-H): a POSIX `ustar` stream of regular files, gzip-compressed.
//!
//! **Deterministic by construction.** Entries are sorted by path; every
//! header carries mode 0644, uid/gid 0, empty owner names and mtime 0; the
//! gzip header carries no name and mtime 0. The same package therefore
//! produces the same bytes on every machine, which is what lets a registry
//! name an archive by its hash and a lockfile pin it.
//!
//! Only what a package is made of is supported: regular files with relative
//! paths. Links, directories and devices are not written, and are refused on
//! read, as is any path that would land outside the destination.

use std::io::{Read, Write};
use std::path::{Component, Path};

const BLOCK: usize = 512;

/// Packs `files` (relative path, contents) into a gzip-compressed ustar
/// archive. Paths use `/` and must fit ustar's 255-byte name.
pub fn pack(files: &[(String, Vec<u8>)]) -> Result<Vec<u8>, String> {
    let mut sorted: Vec<&(String, Vec<u8>)> = files.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));

    let mut tar = Vec::new();
    for (path, bytes) in sorted {
        tar.extend_from_slice(&header(path, bytes.len())?);
        tar.extend_from_slice(bytes);
        tar.resize(tar.len().div_ceil(BLOCK) * BLOCK, 0);
    }
    // End of archive: two zero blocks.
    tar.resize(tar.len() + 2 * BLOCK, 0);

    let mut gz = flate2::GzBuilder::new()
        .mtime(0)
        .write(Vec::new(), flate2::Compression::best());
    gz.write_all(&tar).map_err(|e| e.to_string())?;
    gz.finish().map_err(|e| e.to_string())
}

/// Unpacks an archive written by [`pack`] into `dest`, returning the paths
/// it wrote.
pub fn unpack(archive: &[u8], dest: &Path) -> Result<Vec<String>, String> {
    let mut tar = Vec::new();
    flate2::read::GzDecoder::new(archive)
        .read_to_end(&mut tar)
        .map_err(|e| format!("not a package archive: {e}"))?;

    let mut written = Vec::new();
    let mut at = 0;
    while at + BLOCK <= tar.len() {
        let h = &tar[at..at + BLOCK];
        if h.iter().all(|&b| b == 0) {
            break;
        }
        let name = field_str(&h[0..100]);
        let prefix = field_str(&h[345..500]);
        let path = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        let size = octal(&h[124..136]).ok_or_else(|| format!("`{path}`: bad size"))?;
        let kind = h[156];
        if kind != b'0' && kind != 0 {
            return Err(format!("`{path}`: only regular files belong in a package"));
        }
        if !safe_relative(&path) {
            return Err(format!("`{path}`: a path outside the package"));
        }
        let start = at + BLOCK;
        let end = start + size;
        let bytes = tar
            .get(start..end)
            .ok_or_else(|| format!("`{path}`: archive is truncated"))?;
        let out = dest.join(&path);
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        std::fs::write(&out, bytes).map_err(|e| format!("{}: {e}", out.display()))?;
        written.push(path);
        at = start + size.div_ceil(BLOCK) * BLOCK;
    }
    Ok(written)
}

/// One ustar header for a regular file.
fn header(path: &str, size: usize) -> Result<[u8; BLOCK], String> {
    if !safe_relative(path) {
        return Err(format!(
            "`{path}`: a package path must be relative and inside it"
        ));
    }
    let mut h = [0u8; BLOCK];
    // Split long paths at a `/` into ustar's 155-byte prefix and 100-byte name.
    let (prefix, name) = if path.len() <= 100 {
        ("", path)
    } else {
        let cut = path[..path.len().min(156)]
            .rfind('/')
            .filter(|&i| path.len() - i - 1 <= 100)
            .ok_or_else(|| format!("`{path}`: too long for a package archive"))?;
        (&path[..cut], &path[cut + 1..])
    };
    h[..name.len()].copy_from_slice(name.as_bytes());
    put(&mut h[100..108], "0000644");
    put(&mut h[108..116], "0000000");
    put(&mut h[116..124], "0000000");
    put(&mut h[124..136], &format!("{size:011o}"));
    put(&mut h[136..148], "00000000000");
    h[156] = b'0';
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    h[345..345 + prefix.len()].copy_from_slice(prefix.as_bytes());
    // The checksum is taken with its own field read as spaces.
    h[148..156].fill(b' ');
    let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
    put(&mut h[148..156], &format!("{sum:06o}\0 "));
    Ok(h)
}

fn put(field: &mut [u8], text: &str) {
    field[..text.len()].copy_from_slice(text.as_bytes());
}

fn field_str(field: &[u8]) -> String {
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    String::from_utf8_lossy(&field[..end]).into_owned()
}

fn octal(field: &[u8]) -> Option<usize> {
    let text = field_str(field);
    usize::from_str_radix(text.trim(), 8).ok()
}

/// Relative, `/`-separated, and never climbing out of the destination.
fn safe_relative(path: &str) -> bool {
    !path.is_empty()
        && !path.contains('\\')
        && Path::new(path)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("byard-archive-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_package_round_trips_byte_for_byte() {
        let long = format!("src/{}/deep.byd", "d".repeat(120));
        let files = vec![
            (
                "byard.toml".to_string(),
                b"[package]\nname = \"kit\"\n".to_vec(),
            ),
            ("src/a.byd".to_string(), b"View A() { Box {} }\n".to_vec()),
            (
                "fonts/x.ttf".to_string(),
                (0..=255u8).cycle().take(1500).collect(),
            ),
            (long.clone(), b"View D() {}".to_vec()),
            ("empty.txt".to_string(), Vec::new()),
        ];
        let archive = pack(&files).unwrap();
        let dir = scratch("roundtrip");
        let mut written = unpack(&archive, &dir).unwrap();
        written.sort();
        let mut names: Vec<String> = files.iter().map(|f| f.0.clone()).collect();
        names.sort();
        assert_eq!(written, names);
        for (path, bytes) in &files {
            assert_eq!(&std::fs::read(dir.join(path)).unwrap(), bytes, "{path}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The same files in any order are the same bytes: the property a
    /// content-addressed registry depends on.
    #[test]
    fn packing_is_deterministic() {
        let a = vec![
            ("b.byd".to_string(), b"B".to_vec()),
            ("a.byd".to_string(), b"A".to_vec()),
        ];
        let mut b = a.clone();
        b.reverse();
        assert_eq!(pack(&a).unwrap(), pack(&b).unwrap());
    }

    #[test]
    fn a_path_outside_the_package_is_refused_both_ways() {
        assert!(pack(&[("../evil".to_string(), vec![1])]).is_err());
        assert!(pack(&[("/etc/evil".to_string(), vec![1])]).is_err());
        // A hand-made archive carrying `../evil` does not escape on unpack.
        let mut h = header("okay", 1).unwrap();
        h[..100].fill(0);
        h[..7].copy_from_slice(b"../evil");
        h[148..156].fill(b' ');
        let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
        put(&mut h[148..156], &format!("{sum:06o}\0 "));
        let mut tar = h.to_vec();
        tar.push(b'x');
        tar.resize(4 * BLOCK, 0);
        let mut gz = flate2::GzBuilder::new().write(Vec::new(), flate2::Compression::fast());
        gz.write_all(&tar).unwrap();
        let archive = gz.finish().unwrap();
        let dir = scratch("escape");
        let err = unpack(&archive, &dir.join("inner")).unwrap_err();
        assert!(err.contains("outside"), "{err}");
        assert!(!dir.join("evil").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
