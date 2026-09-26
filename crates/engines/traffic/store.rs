//! 交换存储：录制的 HTTP 交换落 SQLite（WAL），正文超阈值外溢到 blob。
//!
//! 表结构对齐参考实现的索引面（exchanges + bodies 两张表，blob 内容寻址）。
//! MVP 不做 FTS（后续接 traffic_search 时再上 trigram）。

use std::path::Path;
use std::sync::Mutex;


/// 正文内联阈值（超过外溢到 `_blobs/`）。
const MAX_INLINE_BODY: usize = 256 * 1024;

/// 一条录制的 HTTP 交换。
#[derive(Debug, Clone)]
pub struct ExchangeRecord {
    /// 交换 id（`<unix>-<seq>`）。
    pub id: String,
    /// 请求方法。
    pub method: String,
    /// 完整 URL。
    pub url: String,
    /// host（索引用）。
    pub host: String,
    /// 响应状态码。
    pub status: u16,
    /// 请求头原文（CRLF 分隔，可回放）。
    pub req_head: String,
    /// 请求正文。
    pub req_body: Vec<u8>,
    /// 响应头原文。
    pub resp_head: String,
    /// 响应正文。
    pub resp_body: Vec<u8>,
}

/// 交换存储（`_index/index.sqlite` + `_blobs/`）。
pub struct ExchangeStore {
    conn: Mutex<rusqlite::Connection>,
    blobs_dir: std::path::PathBuf,
}

impl ExchangeStore {
    /// 打开（或创建）存储。
    ///
    /// # Errors
    /// 目录创建 / SQL 执行失败。
    pub fn open(dir: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(dir).map_err(|error| format!("create index dir: {error}"))?;
        let blobs_dir = dir.join("_blobs");
        std::fs::create_dir_all(&blobs_dir).map_err(|error| format!("create blobs dir: {error}"))?;
        let conn = rusqlite::Connection::open(dir.join("index.sqlite"))
            .map_err(|error| format!("open index.sqlite: {error}"))?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|error| format!("set WAL: {error}"))?;
        conn.pragma_update(None, "busy_timeout", 5000)
            .map_err(|error| format!("set busy_timeout: {error}"))?;
        // 正文 FTS：contentless + trigram。trigram 支持任意子串匹配
        // （`ssw0r` 找 `P@ssw0rd`）且对 CJK 按字符切分有效；contentless
        // 不落正文副本，只存倒排索引（查询只回 id，正文回 bodies 表取）。
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS exchanges (
                 id TEXT PRIMARY KEY,
                 ts INTEGER NOT NULL,
                 host TEXT NOT NULL,
                 method TEXT NOT NULL,
                 url TEXT NOT NULL,
                 status INTEGER NOT NULL,
                 req_len INTEGER NOT NULL,
                 resp_len INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_exchanges_host ON exchanges(host);
             CREATE INDEX IF NOT EXISTS idx_exchanges_ts ON exchanges(ts);
             CREATE TABLE IF NOT EXISTS exchange_bodies (
                 id TEXT PRIMARY KEY,
                 req_head TEXT NOT NULL,
                 req_body BLOB NOT NULL,
                 resp_head TEXT NOT NULL,
                 resp_body BLOB NOT NULL
             );
             CREATE VIRTUAL TABLE IF NOT EXISTS exchange_fts USING fts5(
                 id UNINDEXED,
                 body,
                 tokenize = 'trigram'
             );",
        )
        .map_err(|error| format!("create schema: {error}"))?;
        Ok(Self {
            conn: Mutex::new(conn),
            blobs_dir,
        })
    }

    /// 落一条交换（正文超阈值外溢 blob，索引行记长度）。
    ///
    /// # Errors
    /// SQL 执行 / 落盘失败。
    pub fn record(&self, record: &ExchangeRecord) -> Result<(), String> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| "store lock poisoned".to_string())?;
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or_default();
        let req_body = self.spill_if_large(&record.id, "req", &record.req_body)?;
        let resp_body = self.spill_if_large(&record.id, "resp", &record.resp_body)?;
        conn.execute(
            "INSERT OR REPLACE INTO exchanges (id, ts, host, method, url, status, req_len, resp_len)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                record.id,
                ts as i64,
                record.host,
                record.method,
                record.url,
                record.status as i64,
                record.req_body.len() as i64,
                record.resp_body.len() as i64,
            ],
        )
        .map_err(|error| format!("insert exchange: {error}"))?;
        conn.execute(
            "INSERT OR REPLACE INTO exchange_bodies (id, req_head, req_body, resp_head, resp_body)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                record.id,
                record.req_head,
                req_body,
                record.resp_head,
                resp_body,
            ],
        )
        .map_err(|error| format!("insert bodies: {error}"))?;
        // 正文进 FTS（contentless 表的特殊 INSERT 语法）；二进制正文跳过
        // （NUL 兜底，与参考实现同向）。
        let fts_body = format!(
            "{}\n{}",
            String::from_utf8_lossy(&record.req_body),
            String::from_utf8_lossy(&record.resp_body)
        );
        if !fts_body.contains('\0') {
            conn.execute(
                "INSERT INTO exchange_fts(id, body) VALUES (?1, ?2)",
                rusqlite::params![record.id, fts_body],
            )
            .map_err(|error| format!("insert fts: {error}"))?;
        }
        Ok(())
    }

    /// 最近 N 条交换的索引行（`(id, method, url, status)`）。
    ///
    /// # Errors
    /// SQL 执行失败。
    pub fn recent(&self, limit: usize) -> Result<Vec<(String, String, String, u16)>, String> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| "store lock poisoned".to_string())?;
        let mut statement = conn
            .prepare(
                "SELECT id, method, url, status FROM exchanges ORDER BY ts DESC, rowid DESC LIMIT ?1",
            )
            .map_err(|error| format!("prepare recent: {error}"))?;
        let rows = statement
            .query_map(rusqlite::params![limit as i64], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, u16>(3)?,
                ))
            })
            .map_err(|error| format!("query recent: {error}"))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|error| format!("read recent row: {error}"))?);
        }
        Ok(out)
    }

    /// 按 host（必填）+ 可选正文子串检索交换，返回轻量索引行
    /// （`(id, method, url, status)`，最新在前）。
    ///
    /// `body_contains` 走 FTS5 trigram（任意子串、CJK 按字符切分）；
    /// 短于 3 字符的条件退化为元数据 LIKE（trigram 最小匹配单元）。
    ///
    /// # Errors
    /// SQL 执行失败。
    pub fn search(
        &self,
        host: &str,
        body_contains: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(String, String, String, u16)>, String> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| "store lock poisoned".to_string())?;
        let body_contains = body_contains
            .map(str::trim)
            .filter(|text| !text.is_empty());
        if let Some(needle) = body_contains {
            if needle.chars().count() >= 3 {
                let mut statement = conn
                    .prepare(
                        "SELECT e.id, e.method, e.url, e.status
                         FROM exchange_fts f
                         JOIN exchanges e ON e.id = f.id
                         WHERE e.host = ?1 AND exchange_fts MATCH ?2
                         ORDER BY e.ts DESC, e.rowid DESC
                         LIMIT ?3",
                    )
                    .map_err(|error| format!("prepare fts search: {error}"))?;
                let rows = statement
                    .query_map(
                        rusqlite::params![host, format!("\"{needle}\"", ), limit as i64],
                        |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, String>(1)?,
                                row.get::<_, String>(2)?,
                                row.get::<_, u16>(3)?,
                            ))
                        },
                    )
                    .map_err(|error| format!("fts search: {error}"))?;
                return rows
                    .map(|row| row.map_err(|error| format!("read fts row: {error}")))
                    .collect();
            }
            let pattern = format!("%{needle}%");
            let mut statement = conn
                .prepare(
                    "SELECT e.id, e.method, e.url, e.status
                     FROM exchanges e
                     JOIN exchange_bodies b ON b.id = e.id
                     WHERE e.host = ?1
                       AND (CAST(b.req_body AS TEXT) LIKE ?2 OR CAST(b.resp_body AS TEXT) LIKE ?2)
                     ORDER BY e.ts DESC, e.rowid DESC
                     LIMIT ?3",
                )
                .map_err(|error| format!("prepare like search: {error}"))?;
            let rows = statement
                .query_map(rusqlite::params![host, pattern, limit as i64], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, u16>(3)?,
                    ))
                })
                .map_err(|error| format!("like search: {error}"))?;
            return rows
                .map(|row| row.map_err(|error| format!("read like row: {error}")))
                .collect();
        }
        let mut statement = conn
            .prepare(
                "SELECT id, method, url, status FROM exchanges
                 WHERE host = ?1 ORDER BY ts DESC, rowid DESC LIMIT ?2",
            )
            .map_err(|error| format!("prepare host search: {error}"))?;
        let rows = statement
            .query_map(rusqlite::params![host, limit as i64], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, u16>(3)?,
                ))
            })
            .map_err(|error| format!("host search: {error}"))?;
        rows.map(|row| row.map_err(|error| format!("read host row: {error}")))
            .collect()
    }

    /// 取一条交换的完整原文（`(req_head, req_body, resp_head, resp_body)`）。
    ///
    /// # Errors
    /// SQL 执行失败。
    pub fn bodies(
        &self,
        id: &str,
    ) -> Result<Option<(String, Vec<u8>, String, Vec<u8>)>, String> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| "store lock poisoned".to_string())?;
        let mut statement = conn
            .prepare(
                "SELECT req_head, req_body, resp_head, resp_body FROM exchange_bodies WHERE id = ?1",
            )
            .map_err(|error| format!("prepare bodies: {error}"))?;
        let row = statement
            .query_row(rusqlite::params![id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                ))
            });
        match row {
            Ok(found) => Ok(Some(found)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(format!("read bodies: {error}")),
        }
    }

    /// 正文超阈值时外溢 blob，返回内联内容（外溢则返回占位说明）。
    fn spill_if_large(&self, id: &str, kind: &str, body: &[u8]) -> Result<Vec<u8>, String> {
        let _ = id;
        if body.len() <= MAX_INLINE_BODY {
            return Ok(body.to_vec());
        }
        use sha2::Digest as _;
        let hash = format!("{:x}", sha2::Sha256::digest(body));
        let dir = self.blobs_dir.join(&hash[..2.min(hash.len())]);
        std::fs::create_dir_all(&dir).map_err(|error| format!("create blob dir: {error}"))?;
        std::fs::write(dir.join(&hash), body).map_err(|error| format!("write blob: {error}"))?;
        Ok(format!("[blob {kind} sha256:{hash} {} bytes]", body.len()).into_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_records_and_lists_exchanges() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ExchangeStore::open(dir.path()).expect("store opens");
        store
            .record(&ExchangeRecord {
                id: "1-0001".to_string(),
                method: "GET".to_string(),
                url: "https://example.test/".to_string(),
                host: "example.test".to_string(),
                status: 200,
                req_head: "GET / HTTP/1.1\r\nHost: example.test\r\n".to_string(),
                req_body: Vec::new(),
                resp_head: "HTTP/1.1 200 OK\r\n".to_string(),
                resp_body: b"<html>ok</html>".to_vec(),
            })
            .expect("record");
        let recent = store.recent(10).expect("recent");
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].2, "https://example.test/");
        assert_eq!(recent[0].3, 200);
    }

    #[test]
    fn store_searches_by_host_and_body() {
        let dir = tempfile::tempdir().expect("tempdir");
        let index = dir.path().join("_index");
        let store = ExchangeStore::open(&index).expect("store opens");
        for (id, host, body) in [
            ("1-0001", "example.test", b"<html>hello world</html>".to_vec()),
            ("2-0001", "other.test", b"{\"password\":\"P@ssw0rd\"}".to_vec()),
        ] {
            store
                .record(&ExchangeRecord {
                    id: id.to_string(),
                    method: "GET".to_string(),
                    url: format!("https://{host}/"),
                    host: host.to_string(),
                    status: 200,
                    req_head: String::new(),
                    req_body: Vec::new(),
                    resp_head: String::new(),
                    resp_body: body,
                })
                .expect("record");
        }

        // host 必填过滤。
        let rows = store.search("example.test", None, 10).expect("search");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].2, "https://example.test/");

        // 正文子串走 FTS（trigram 任意子串）。
        let hit = store
            .search("other.test", Some("ssw0rd"), 10)
            .expect("fts search");
        assert_eq!(hit.len(), 1, "trigram 必须命中 P@ssw0rd 中间片段");
        assert_eq!(hit[0].2, "https://other.test/");

        // 短于 3 字符退化 LIKE，仍可用。
        let short = store
            .search("example.test", Some("el"), 10)
            .expect("like search");
        assert_eq!(short.len(), 1);

        // 全文读取。
        let (req_head, req_body, resp_head, resp_body) =
            store.bodies("2-0001").expect("bodies").expect("row");
        assert!(req_head.is_empty() && req_body.is_empty());
        assert!(resp_head.is_empty());
        assert_eq!(resp_body, b"{\"password\":\"P@ssw0rd\"}".to_vec());
        assert!(store.bodies("missing").expect("bodies").is_none());
    }

    #[test]
    fn large_body_spills_to_blob() {
        let dir = tempfile::tempdir().expect("tempdir");
        let index = dir.path().join("_index");
        let store = ExchangeStore::open(&index).expect("store opens");
        let big = vec![b'a'; MAX_INLINE_BODY + 10];
        store
            .record(&ExchangeRecord {
                id: "2-0001".to_string(),
                method: "POST".to_string(),
                url: "https://example.test/upload".to_string(),
                host: "example.test".to_string(),
                status: 200,
                req_head: String::new(),
                req_body: big,
                resp_head: String::new(),
                resp_body: Vec::new(),
            })
            .expect("record");
        // 索引行记原始长度，正文列是占位说明。
        let recent = store.recent(1).expect("recent");
        assert_eq!(recent[0].2, "https://example.test/upload");
        assert!(
            std::fs::read_dir(index.join("_blobs"))
                .expect("blobs dir")
                .count()
                > 0,
            "大正文必须外溢"
        );
    }
}
