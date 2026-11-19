//! Synthetic, dependency-free reproduction aid for #2270. Never pass a database.
//! Compile with the repository toolchain; argv[1] must name a fresh directory.
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    thread,
};

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.0) {
            eprintln!("probe cleanup failed: {error}");
        }
    }
}

fn copy(source: &Path, target: &Path, method: &str) -> io::Result<u64> {
    if method == "fs-copy" {
        return fs::copy(source, target);
    }
    let mut input = File::open(source)?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)?;
    let count = if method == "file-stream" {
        io::copy(&mut input, &mut output)?
    } else {
        let mut bytes = Vec::new();
        input.read_to_end(&mut bytes)?;
        output.write_all(&bytes)?;
        bytes.len() as u64
    };
    output.sync_all()?;
    Ok(count)
}

#[derive(Debug)]
struct Observation {
    returned_count_matches: bool,
    source_matches: bool,
    destination_matches: bool,
    destination_reads_agree: bool,
    destination_all_zero: bool,
}

impl Observation {
    fn passed(&self) -> bool {
        self.returned_count_matches
            && self.source_matches
            && self.destination_matches
            && self.destination_reads_agree
    }
}

fn observe(expected: &[u8], count: u64, source: &[u8], first: &[u8], second: &[u8]) -> Observation {
    Observation {
        returned_count_matches: count == expected.len() as u64,
        source_matches: source == expected,
        destination_matches: first == expected,
        destination_reads_agree: first == second,
        destination_all_zero: !first.is_empty() && first.iter().all(|byte| *byte == 0),
    }
}

fn worker(root: &Path, id: usize) -> io::Result<(usize, usize)> {
    let root = root.join(format!("worker-{id}"));
    fs::create_dir(&root)?;
    let mut checked = 0;
    let mut failed = 0;
    for round in 0..32 {
        for preparation in ["fresh", "readback", "synced"] {
            for method in ["fs-copy", "file-stream", "buffered-control"] {
                for expected in [b"legacy-main".as_slice(), b"legacy-wal-rows", b"legacy-shm"] {
                    let source = root.join("source");
                    let target = root.join("target");
                    // Re-seed for every method. Reading or syncing before fs::copy
                    // could mask a dirty-source problem, so fresh does neither.
                    fs::write(&source, expected)?;
                    if preparation == "synced" {
                        File::open(&source)?.sync_all()?;
                    } else if preparation == "readback" && fs::read(&source)? != expected {
                        return Err(io::Error::other("source seed readback differs before copy"));
                    }
                    let copied = copy(&source, &target, method);
                    let first = fs::read(&target);
                    let source_after = fs::read(&source);
                    let second = fs::read(&target);
                    let observation = match (&copied, &source_after, &first, &second) {
                        (Ok(n), Ok(src), Ok(a), Ok(b)) => Some(observe(expected, *n, src, a, b)),
                        _ => None,
                    };
                    checked += 1;
                    if !observation.as_ref().is_some_and(Observation::passed) {
                        failed += 1;
                        if failed <= 3 {
                            let all_zero = observation.as_ref().map(|o| o.destination_all_zero);
                            eprintln!("MISMATCH worker={id} round={round} preparation={preparation} method={method} expected={expected:?} copied={copied:?} all_zero={all_zero:?} observation={observation:?} source_after={source_after:?} destination_first={first:?} destination_second={second:?} source_metadata={:?} destination_metadata={:?}", fs::metadata(&source), fs::metadata(&target));
                        }
                    }
                    fs::remove_file(&source)?;
                    if target.exists() {
                        fs::remove_file(&target)?;
                    }
                }
            }
        }
    }
    Ok((checked, failed))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args_os()
        .nth(1)
        .ok_or("usage: legacy-copy-probe <fresh-directory>")?;
    let root = PathBuf::from(path);
    fs::create_dir(&root)?;
    let scratch = Scratch(root);
    let outcomes = thread::scope(|scope| {
        let handles: Vec<_> = (0..4)
            .map(|id| {
                let root = &scratch.0;
                scope.spawn(move || worker(root, id))
            })
            .collect();
        // Join every worker, including after an error, before scratch cleanup.
        handles
            .into_iter()
            .map(|handle| handle.join())
            .collect::<Vec<_>>()
    });
    let mut totals = (0, 0);
    for outcome in outcomes {
        let (checked, failed) = outcome.map_err(|_| "probe worker panicked")??;
        totals.0 += checked;
        totals.1 += failed;
    }
    println!("legacy-copy-probe checked={} mismatches={} workers=4 rounds=32 methods=fs-copy,file-stream,buffered-control source_preparation=fresh,readback,synced", totals.0, totals.1);
    if totals.1 > 0 {
        return Err("synthetic copy mismatch; inspect observations above".into());
    }
    println!("No mismatch in this probe; this does not explain or dismiss the historical failure.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_length_zero_destination_is_distinct_from_changed_source() {
        let original = b"legacy-main";
        let zero = [0; 11];
        let destination_fault = observe(original, 11, original, &zero, &zero);
        assert!(!destination_fault.passed());
        assert!(destination_fault.source_matches);
        assert!(destination_fault.destination_all_zero);
        let source_fault = observe(original, 11, &zero, &zero, &zero);
        assert!(!source_fault.passed());
        assert!(!source_fault.source_matches);
    }

    #[test]
    fn second_read_drift_and_short_copy_cannot_pass() {
        let original = b"legacy-main";
        assert!(!observe(original, 11, original, original, &[0; 11]).passed());
        assert!(!observe(original, 10, original, original, original).passed());
        assert!(observe(original, 11, original, original, original).passed());
    }
}
