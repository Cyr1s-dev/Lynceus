//! 一次性探测：bundled SQLite 是否带 FTS5，以及 unicode61 对 CJK 的分词行为。
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[test]
fn fts5_available_in_bundled_sqlite() {
    let connection = rusqlite::Connection::open_in_memory().expect("内存库必须可创建");
    connection
        .execute(
            "CREATE VIRTUAL TABLE probe USING fts5(t, tokenize = 'unicode61')",
            [],
        )
        .expect("bundled SQLite 必须带 FTS5");
    connection
        .execute("INSERT INTO probe VALUES ('文件上传 RCE')", [])
        .expect("写入必须成功");
    let hit: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM probe WHERE probe MATCH '\"上传\"'",
            [],
            |row| row.get(0),
        )
        .expect("MATCH 必须可执行");
    assert_eq!(
        hit, 0,
        "unicode61 把连续 CJK 当整段 token，跨字词不得命中——验证分词假设"
    );
    let whole: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM probe WHERE probe MATCH '\"文件上传\"'",
            [],
            |row| row.get(0),
        )
        .expect("MATCH 必须可执行");
    assert_eq!(whole, 1, "整段 token 应命中");
}
