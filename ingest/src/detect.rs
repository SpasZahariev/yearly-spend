#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Neon,
    Revolut,
    Cashback,
    Unknown(String),
}

impl Source {
    pub fn name(&self) -> &str {
        match self {
            Source::Neon => "neon",
            Source::Revolut => "revolut",
            Source::Cashback => "cashback",
            Source::Unknown(n) => n,
        }
    }

    #[allow(dead_code)]
    pub fn is_known(&self) -> bool {
        matches!(self, Source::Neon | Source::Revolut | Source::Cashback)
    }
}

/// The statement source is auto-detected from the nearest ancestor directory
/// named after a source (`neon` | `revolut` | `cashback_cards`), matched
/// case-insensitively, as agreed in the spec.
pub fn detect_source(path: &std::path::Path) -> Source {
    for ancestor in path.ancestors() {
        let Some(name) = ancestor.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let lower = name.to_ascii_lowercase();
        match lower.as_str() {
            "neon" => return Source::Neon,
            "revolut" => return Source::Revolut,
            "cashback_cards" | "cashback" => return Source::Cashback,
            _ => {}
        }
    }
    let parent = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("");
    Source::Unknown(parent.to_string())
}

fn is_csv(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("csv"))
}

/// Collect every `.csv` under the given files/directories, recursively.
/// Results are sorted and de-duplicated for a stable, idempotent listing.
pub fn collect_csvs(paths: &[std::path::PathBuf]) -> anyhow::Result<Vec<std::path::PathBuf>> {
    let mut out = Vec::new();
    for p in paths {
        if p.is_dir() {
            collect_dir(p, &mut out)?;
        } else if p.is_file() {
            out.push(p.clone());
        } else {
            anyhow::bail!("path does not exist: {}", p.display());
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

fn collect_dir(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let p = entry.path();
        if p.is_dir() {
            collect_dir(&p, out)?;
        } else if is_csv(&p) {
            out.push(p);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    fn root() -> std::path::PathBuf {
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("spend-detect-{}-{}", std::process::id(), n));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("neon")).unwrap();
        std::fs::create_dir_all(dir.join("revolut/sub")).unwrap();
        std::fs::create_dir_all(dir.join("cashback_cards")).unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        let mut f = std::fs::File::create(dir.join("neon/a.csv")).unwrap();
        let _ = f.write_all(b"x");
        let mut f = std::fs::File::create(dir.join("revolut/sub/b.csv")).unwrap();
        let _ = f.write_all(b"x");
        let mut f = std::fs::File::create(dir.join("cashback_cards/c.csv")).unwrap();
        let _ = f.write_all(b"x");
        let mut f = std::fs::File::create(dir.join("notes.txt")).unwrap();
        let _ = f.write_all(b"x");
        dir
    }

    #[test]
    fn detects_known_sources() {
        let r = root();
        assert_eq!(detect_source(&r.join("neon/a.csv")), Source::Neon);
        assert_eq!(detect_source(&r.join("revolut/sub/b.csv")), Source::Revolut);
        assert_eq!(
            detect_source(&r.join("cashback_cards/c.csv")),
            Source::Cashback
        );
        assert!(!detect_source(&r.join("notes.txt")).is_known());
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn detects_sources_case_insensitively_in_nested_dirs() {
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("spend-detect2-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Neon")).unwrap();
        std::fs::create_dir_all(dir.join("Revolut/2025")).unwrap();
        std::fs::File::create(dir.join("Neon/a.csv")).unwrap();
        std::fs::File::create(dir.join("Revolut/2025/b.csv")).unwrap();
        assert_eq!(detect_source(&dir.join("Neon/a.csv")), Source::Neon);
        assert_eq!(
            detect_source(&dir.join("Revolut/2025/b.csv")),
            Source::Revolut
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn collects_csvs_recursively_and_skips_other_files() {
        let r = root();
        let files = collect_csvs(std::slice::from_ref(&r)).unwrap();
        let names: Vec<String> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        // sorted by full path: cashback_cards < neon < revolut
        assert_eq!(names, vec!["c.csv", "a.csv", "b.csv"]);
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn missing_path_is_an_error() {
        let r = root();
        assert!(collect_csvs(&[r.join("nope.csv")]).is_err());
        let _ = std::fs::remove_dir_all(&r);
    }
}
