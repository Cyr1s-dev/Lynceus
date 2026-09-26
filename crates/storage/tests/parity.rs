//! Rust-only repository golden test.
//!
//! A deterministic [`storage::fixture`] operation sequence is dumped
//! and compared byte-for-byte with the repository's frozen contract fixture.
//! The fixture is embedded at compile time so the test has no runtime path or
//! Python generator dependency.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use storage::SqliteRepository;
use storage::dump_database;
use storage::fixture;

const GOLDEN: &str = include_str!("fixtures/parity_golden.json");

#[test]
fn rust_fixture_dump_matches_frozen_golden_bytes() {
    let dir = tempfile::tempdir().expect("系统临时目录应可创建");
    let database = dir.path().join("parity.sqlite3");

    let repo = SqliteRepository::open(&database).expect("fixture 数据库必须可打开");
    let snapshot = fixture::build(&repo).expect("固定操作序列必须成功");
    drop(repo);

    // 语义断言：读取方法的行为与 fixture 预期一致（不止是落盘字节）。
    fixture::verify(&snapshot).expect("fixture 语义校验必须通过");

    let dump = dump_database(&database).expect("Rust 建的库必须可 dump");
    let actual = format!("{dump}\n");
    let expected = format!("{}\n", GOLDEN.trim_end());
    assert_eq!(
        actual, expected,
        "Rust fixture dump drifted from the frozen storage wire contract"
    );
}
