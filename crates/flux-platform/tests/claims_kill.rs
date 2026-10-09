//! The crash ACCEPTANCE GATE for the claim store: a writer killed mid-stream must leave a
//! file that reopens and holds a prefix of the claims, never a hole.

use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use flux_fs::{ClaimKey, ClaimRecord, ClaimStatus, ClaimStore, Durability, FluxPathKey, ObjectId};
use flux_platform::{RedbClaimStore, SYNC_CAP};
use redb::{Builder, ReadableDatabase, ReadableTable, TableDefinition};

const CLAIMS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("claims");
const PARENT: ObjectId = ObjectId { volume: 1, index: 7 };

const THRESHOLDS: [u64; 24] = [
    1, 2, 3, 7, 49, 50, 51, 99, 100, 101, 149, 150, 333, 1000, 1001, 1500, 2000, 3, 60, 120, 240,
    480, 960, 1920,
];
const MODES: [&str; 3] = ["normal", "normal-nocap-test", "strict"];
/// The largest threshold `strict` mode is run at; larger ones are skipped for `strict` only.
///
/// Strict commits are `Immediate` on every claim, so each claim is an fsync, and past this point
/// the extra claims cost minutes on Windows and add no new coverage: the cap-sync behaviour at
/// 1000 and above is exercised by `normal-nocap-test`, which keeps every threshold. The owner
/// approved this gate change in 2026-10 (cut 8b); `normal` and `normal-nocap-test` are unchanged.
const STRICT_MAX_THRESHOLD: u64 = 332;

fn key_for(i: u64) -> ClaimKey {
    ClaimKey { parent: PARENT, name: i.to_be_bytes().to_vec() }
}

#[test]
#[ignore]
fn child_writer() {
    let path = std::env::var("FLUX_KILL_FILE").unwrap();
    let mode = std::env::var("FLUX_KILL_MODE").unwrap();
    let file = OpenOptions::new().read(true).write(true).create_new(true).open(path).unwrap();
    let durability = if mode == "strict" { Durability::Strict } else { Durability::Normal };
    let mut store = RedbClaimStore::create_file(file, durability).unwrap();
    let rec = ClaimRecord { target: FluxPathKey(b"t".to_vec()), status: ClaimStatus::Existing };
    let stdout = std::io::stdout();
    let mut i: u64 = 0;
    loop {
        store.insert_if_absent(&key_for(i), &rec).unwrap();
        {
            let mut o = stdout.lock();
            writeln!(o, "C {i}").unwrap();
            o.flush().unwrap();
        }
        if mode == "normal" && i % 50 == 49 {
            store.flush().unwrap();
            let mut o = stdout.lock();
            writeln!(o, "F {i}").unwrap();
            o.flush().unwrap();
        }
        i += 1;
    }
}

#[test]
fn killed_mid_stream_the_store_reopens_with_a_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let exe = std::env::current_exe().unwrap();
    let mut failures = Vec::new();
    println!("{:<18} {:>9} {:>7} {:>7} {:>7}", "mode", "threshold", "m", "lastF", "lastC");
    let mut run = 0u32;
    for &threshold in &THRESHOLDS {
        for mode in MODES {
            if mode == "strict" && threshold > STRICT_MAX_THRESHOLD {
                continue;
            }
            run += 1;
            let path = dir.path().join(format!("{run}-{mode}-{threshold}.db"));
            let mut child = Command::new(&exe)
                .args(["--exact", "child_writer", "--ignored", "--nocapture", "--test-threads=1"])
                .env("FLUX_KILL_FILE", &path)
                .env("FLUX_KILL_MODE", mode)
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap();
            let mut reader = BufReader::new(child.stdout.take().unwrap());
            let (mut c_count, mut last_c, mut last_f) = (0u64, None::<u64>, None::<u64>);
            let mut line = String::new();
            while c_count < threshold {
                line.clear();
                if reader.read_line(&mut line).unwrap() == 0 {
                    break;
                }
                let mut it = line.split_whitespace();
                match (it.next(), it.next().and_then(|n| n.parse::<u64>().ok())) {
                    (Some("C"), Some(n)) => {
                        c_count += 1;
                        last_c = Some(n);
                    }
                    (Some("F"), Some(n)) => last_f = Some(n),
                    _ => {}
                }
            }
            child.kill().unwrap();
            child.wait().unwrap();
            assert_eq!(c_count, threshold, "{mode}/{threshold}: child ended early");

            let file = OpenOptions::new().read(true).write(true).open(&path).unwrap();
            let db = Builder::new().create_file(file).expect("the file must open after a kill");
            let tx = db.begin_read().unwrap();
            let table = tx.open_table(CLAIMS).unwrap();
            let mut present = Vec::new();
            for entry in table.iter().unwrap() {
                let (k, _) = entry.unwrap();
                let name = &k.value()[24..];
                present.push(u64::from_be_bytes(name.try_into().unwrap()));
            }
            present.sort_unstable();
            let m = present.len() as u64;
            drop(table);
            drop(tx);
            drop(db);
            // The engine path: reopen through `open_file`, then keep claiming.
            let file = OpenOptions::new().read(true).write(true).open(&path).unwrap();
            let mut reopened = RedbClaimStore::open_file(file, Durability::Strict)
                .expect("open_file must reopen a killed store");
            assert_eq!(reopened.count().unwrap(), m, "{mode}/{threshold}: count after reopen");
            let rec =
                ClaimRecord { target: FluxPathKey(b"x".to_vec()), status: ClaimStatus::Created };
            reopened.insert_if_absent(&key_for(u64::MAX), &rec).unwrap();
            assert_eq!(reopened.count().unwrap(), m + 1, "{mode}/{threshold}: count after insert");
            drop(reopened);
            let prefix = present.iter().enumerate().all(|(idx, &v)| v == idx as u64);
            let floor = match mode {
                "normal" => last_f.map_or(0, |i| i + 1),
                "strict" => last_c.map_or(0, |i| i + 1),
                _ => {
                    let cap = u64::from(SYNC_CAP);
                    cap * ((last_c.map_or(0, |i| i + 1)) / cap)
                }
            };
            println!(
                "{mode:<18} {threshold:>9} {m:>7} {:>7} {:>7}",
                last_f.map_or("-".to_string(), |v| v.to_string()),
                last_c.map_or("-".to_string(), |v| v.to_string()),
            );
            if !prefix {
                failures.push(format!("{mode}/{threshold}: hole in prefix (m = {m})"));
            }
            if m < floor {
                failures.push(format!("{mode}/{threshold}: m = {m} < required {floor}"));
            }
        }
    }
    assert!(failures.is_empty(), "ACCEPTANCE_FAILED: {failures:#?}");
}

#[test]
fn a_store_builds_from_an_already_open_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.db");
    let file =
        std::fs::OpenOptions::new().read(true).write(true).create_new(true).open(&path).unwrap();
    let mut store = RedbClaimStore::create_file(file, Durability::Strict).unwrap();
    let key = key_for(5);
    let rec = ClaimRecord { target: FluxPathKey(b"x".to_vec()), status: ClaimStatus::Created };
    store.insert_if_absent(&key, &rec).unwrap();
    drop(store);

    let db = redb::Database::open(&path).unwrap();
    let tx = db.begin_read().unwrap();
    let table = tx.open_table(CLAIMS).unwrap();
    let got = table.get(key.encode().as_slice()).unwrap().unwrap();
    assert_eq!(ClaimRecord::decode(got.value()), Some(rec));
}

fn note_for(i: u64) -> flux_fs::PreparedRecord {
    flux_fs::PreparedRecord {
        target: FluxPathKey(i.to_be_bytes().to_vec()),
        temp_name: b"n.flux-partial.x".to_vec(),
        identity: "strong:1:9".to_string(),
        dir_path: String::new(),
        name: i.to_be_bytes().to_vec(),
        planned_name: Vec::new(),
        replacement: false,
    }
}

#[test]
#[ignore]
fn child_note_writer() {
    let path = std::env::var("FLUX_KILL_FILE").unwrap();
    let file = OpenOptions::new().read(true).write(true).create_new(true).open(path).unwrap();
    let mut store = RedbClaimStore::create_file(file, Durability::Normal).unwrap();
    let stdout = std::io::stdout();
    let mut i: u64 = 0;
    loop {
        store.prepare(&key_for(i), &note_for(i)).unwrap();
        {
            let mut o = stdout.lock();
            writeln!(o, "P {i}").unwrap();
            o.flush().unwrap();
        }
        if i.is_multiple_of(2) {
            store
                .commit_prepared(&key_for(i), &FluxPathKey(i.to_be_bytes().to_vec()), None)
                .unwrap();
            let mut o = stdout.lock();
            writeln!(o, "K {i}").unwrap();
            o.flush().unwrap();
        }
        i += 1;
    }
}

#[test]
fn a_killed_writer_keeps_every_prepared_note_under_normal() {
    let dir = tempfile::tempdir().unwrap();
    let exe = std::env::current_exe().unwrap();
    for (run, &threshold) in [1u64, 2, 3, 7, 20, 51, 100].iter().enumerate() {
        let path = dir.path().join(format!("{run}.db"));
        let mut child = Command::new(&exe)
            .args(["--exact", "child_note_writer", "--ignored", "--nocapture", "--test-threads=1"])
            .env("FLUX_KILL_FILE", &path)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let mut reader = BufReader::new(child.stdout.take().unwrap());
        let (mut p_seen, mut committed) = (Vec::new(), Vec::new());
        let mut line = String::new();
        while (p_seen.len() as u64) < threshold {
            line.clear();
            if reader.read_line(&mut line).unwrap() == 0 {
                break;
            }
            let mut it = line.split_whitespace();
            match (it.next(), it.next().and_then(|n| n.parse::<u64>().ok())) {
                (Some("P"), Some(n)) => p_seen.push(n),
                (Some("K"), Some(n)) => committed.push(n),
                _ => {}
            }
        }
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(p_seen.len() as u64, threshold, "{threshold}: child ended early");

        let file = OpenOptions::new().read(true).write(true).open(&path).unwrap();
        let store = RedbClaimStore::open_file(file, Durability::Strict)
            .expect("open_file must reopen a killed store");
        let listed: Vec<ClaimKey> = store.prepared().unwrap().into_iter().map(|(k, _)| k).collect();
        for &i in &p_seen {
            let key = key_for(i);
            let has_note = listed.contains(&key);
            let claim = store.get(&key).unwrap();
            let want = ClaimRecord {
                target: FluxPathKey(i.to_be_bytes().to_vec()),
                status: ClaimStatus::Created,
            };
            match (has_note, claim) {
                (true, None) => {
                    assert!(!committed.contains(&i), "{threshold}/{i}: acknowledged commit lost")
                }
                (false, Some(c)) => assert_eq!(c, want, "{threshold}/{i}: committed claim"),
                (true, Some(_)) => panic!("{threshold}/{i}: both the note and the claim"),
                (false, None) => panic!("{threshold}/{i}: neither the note nor the claim"),
            }
        }
    }
}
