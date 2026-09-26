//! 共享原语 —— `server/core/models/common.py` 对应部分的移植。
//!
//! 提供 Python 侧 `new_id` / `utcnow` 的镜像与 [`Timestamp`] 类型。
//! [`Timestamp`] 是本 crate 全部时间字段的唯一载体，它的 serde 实现逐字节
//! 复刻 pydantic v2 的 datetime wire 格式（见类型文档），这是差分对拍
//! “payload 逐字节一致”的前提之一。

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, FixedOffset, SubsecRound, Timelike, Utc};
use indexmap::IndexMap;
use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;
use serde::Serializer;
use serde::de::Visitor;

/// Python `new_id`：`"{prefix}_{uuid4().hex[:12]}"` 的镜像。
///
/// 前缀让 ID 在日志与 SARIF 中自描述（如 `fact_3f2a…`）。
#[must_use]
pub fn new_id(prefix: &str) -> String {
    let hex = uuid::Uuid::new_v4().simple().to_string();
    format!("{prefix}_{}", &hex[..12])
}

/// Python `utcnow`：时区感知的当前 UTC 时间的镜像。
///
/// Python `datetime.now` 只有微秒精度；chrono 在部分平台给纳秒，这里
/// 截断到微秒，保证往返序列化与直接相等比较都与 Python 一致。
#[must_use]
pub fn utcnow() -> Timestamp {
    Timestamp(Utc::now().trunc_subsecs(6).fixed_offset())
}

/// Python `datetime`（tz-aware，微秒精度）的镜像。
///
/// # wire 格式（serde 实现，与 pydantic v2 逐字节一致）
///
/// - 日期时间主体：`YYYY-MM-DDTHH:MM:SS`；
/// - 微秒：**非零**时恒为 6 位小数（`.123000` 不折叠为 `.123`），为零时省略；
/// - 偏移：UTC（偏移为 0）序列化为 `Z`，其他偏移保留为 `±HH:MM`
///   （pydantic 不做时区归一，`-07:00` 原样往返）；
/// - 解析侧接受 pydantic 接受的变体：`T`/`t`/空格分隔、`Z`/`z`、1–9 位
///   小数（截断到微秒，与 pydantic 一致）；闰秒（`:60`）被拒绝，因为
///   Python `datetime` 无法表示。
///
/// 相等语义与 Python 一致：**按时刻比较**（同一时刻不同偏移视为相等）。
#[derive(Debug, Clone, Copy)]
pub struct Timestamp(DateTime<FixedOffset>);

impl Timestamp {
    /// 当前 UTC 时间（等价 Python `utcnow()`）。
    ///
    /// [`utcnow`] 是同名自由函数；此方法便于类型上下文中链式调用。
    #[must_use]
    pub fn now() -> Self {
        utcnow()
    }

    /// `created_at` 列的存储格式：Python `datetime.isoformat()` 的镜像。
    ///
    /// 与 wire 格式的差异：UTC 写作 `+00:00` 而非 `Z`（stdlib `isoformat`
    /// 不做 `Z` 替换）。存储层写 `created_at` 列必须用此格式，写 payload
    /// 必须用 serde 实现，两者不可混用。
    #[must_use]
    pub fn isoformat(&self) -> String {
        format!("{}{}", self.body(), self.0.format("%:z"))
    }

    /// 序列化主体（不含偏移后缀），微秒非零时追加 6 位小数。
    fn body(&self) -> String {
        let base = self.0.format("%Y-%m-%dT%H:%M:%S").to_string();
        let micros = self.0.nanosecond() / 1_000;
        if micros == 0 {
            base
        } else {
            format!("{base}.{micros:06}")
        }
    }

    /// wire 格式字符串（serde 序列化与 `Display` 共用）。
    #[must_use]
    pub fn to_wire_string(&self) -> String {
        let suffix = if self.0.offset().local_minus_utc() == 0 {
            "Z".to_string()
        } else {
            self.0.format("%:z").to_string()
        };
        format!("{}{}", self.body(), suffix)
    }

    /// Mission workspace 目录名时间戳（Python `strftime("%Y%m%d-%H%M%S")`）。
    ///
    /// 仅用于目录命名；列存储与 wire 各自使用 [`Timestamp::isoformat`] /
    /// [`Timestamp::to_wire_string`]，三者不可混用。
    #[must_use]
    pub fn workspace_stamp(&self) -> String {
        self.0.format("%Y%m%d-%H%M%S").to_string()
    }

    /// 返回 `self - earlier` 的非负毫秒数。
    ///
    /// 工具/模型调用审计只接受非负耗时；系统时钟回拨时与 Python 的
    /// `max(0, int(...))` 一致钳制为零。
    #[must_use]
    pub fn elapsed_milliseconds_since(&self, earlier: &Self) -> i64 {
        self.0
            .with_timezone(&Utc)
            .signed_duration_since(earlier.0.with_timezone(&Utc))
            .num_milliseconds()
            .max(0)
    }

    /// 解析 pydantic 可接受的 RFC 3339 变体，截断到微秒精度。
    fn parse_relaxed(text: &str) -> Result<Self, TimestampParseError> {
        let rejected = || TimestampParseError {
            text: text.chars().take(64).collect(),
        };
        // Python datetime.fromisoformat 接受空格分隔符，chrono 的 RFC 3339
        // 解析不接受；规范化为 'T' 后再解析。
        let normalized = if text.as_bytes().get(10) == Some(&b' ') {
            let mut bytes = text.as_bytes().to_vec();
            bytes[10] = b'T';
            // 刚才逐字节替换了 ASCII 空格，输入必然仍是合法 UTF-8。
            String::from_utf8(bytes).map_err(|_| rejected())?
        } else {
            text.to_string()
        };
        let parsed = DateTime::parse_from_rfc3339(&normalized).map_err(|_| rejected())?;
        // Python datetime 无纳秒位：pydantic 解析 7-9 位小数时截断到微秒。
        let nanos = parsed.nanosecond();
        if nanos >= 1_000_000_000 {
            // 闰秒表示（:60 被 chrono 编码为 nanosecond >= 1e9）；
            // Python datetime 无法表示闰秒，pydantic 同样拒绝。
            return Err(rejected());
        }
        let truncated = parsed
            .with_nanosecond(nanos / 1_000 * 1_000)
            .unwrap_or(parsed);
        Ok(Self(truncated))
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_wire_string())
    }
}

/// Python 按时刻比较 aware datetime；这里镜像同一语义。
impl PartialEq for Timestamp {
    fn eq(&self, other: &Self) -> bool {
        self.0.with_timezone(&Utc) == other.0.with_timezone(&Utc)
    }
}

impl Eq for Timestamp {}

/// 按时刻比较（与 [`PartialEq`] 同一语义）；熔断器的
/// `circuit_open_until > utcnow()` 判定依赖此实现。
impl PartialOrd for Timestamp {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// 全序（与 [`PartialOrd`] 同一语义）：Python aware datetime 可全序排序
/// （`sorted(key=lambda item: item.updated_at)` / `max(runs, key=...)` 的
/// 依赖），此处对齐。
impl Ord for Timestamp {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.with_timezone(&Utc).cmp(&other.0.with_timezone(&Utc))
    }
}

/// `timestamp + timedelta`（Python aware datetime 加法语义，偏移量保留）。
impl std::ops::Add<chrono::Duration> for Timestamp {
    type Output = Timestamp;

    fn add(self, rhs: chrono::Duration) -> Self::Output {
        // 溢出（极端年份）饱和到原时刻：Python 侧会抛 OverflowError，
        // 但 cooldown_seconds 经校验为小整数，该分支实际不可达。
        Self(self.0.checked_add_signed(rhs).unwrap_or(self.0))
    }
}

impl FromStr for Timestamp {
    type Err = TimestampParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse_relaxed(s)
    }
}

/// 时间字符串无法按 pydantic 可接受的格式解析。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("not a pydantic-compatible RFC 3339 datetime: {text}")]
pub struct TimestampParseError {
    /// 解析失败的原字符串（截断到 64 字符避免错误信息爆炸）。
    text: String,
}

impl Serialize for Timestamp {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_wire_string())
    }
}

impl<'de> Deserialize<'de> for Timestamp {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct TimestampVisitor;

        impl Visitor<'_> for TimestampVisitor {
            type Value = Timestamp;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a pydantic-compatible RFC 3339 datetime string")
            }

            fn visit_str<E: serde::de::Error>(self, text: &str) -> Result<Self::Value, E> {
                Timestamp::parse_relaxed(text)
                    .map_err(|error| E::custom(format!("{error}: {text}")))
            }

            fn visit_string<E: serde::de::Error>(self, text: String) -> Result<Self::Value, E> {
                self.visit_str(&text)
            }
        }

        deserializer.deserialize_str(TimestampVisitor)
    }
}

/// Python `dict[str, str]` 的镜像：键序保持插入序，值恒为字符串。
///
/// `serde_json::Map<String, Value>` 无法表达“值必须是字符串”这一
/// pydantic 约束（非字符串值会被静默接受，造成两侧解析行为漂移），
/// 因此域内所有 `dict[str, str]` 字段统一使用本类型：
///
/// - 序列化为 JSON object，键序 = 插入序（与 Python dict 一致）；
/// - 反序列化时遇到非字符串值报错（与 pydantic 一致）；
/// - 相等比较按字典语义（顺序无关，与 Python `dict.__eq__` 一致）；
/// - `insert` 覆盖已有键时保持该键的原始位置（与 Python 赋值语义一致）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StrMap {
    entries: IndexMap<String, String>,
}

impl StrMap {
    /// 空映射。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 插入或覆盖键值，返回被覆盖的旧值。
    ///
    /// 覆盖已有键时保留该键首次插入的位置，镜像 Python `dict[k] = v`。
    pub fn insert(&mut self, key: String, value: String) -> Option<String> {
        self.entries.insert(key, value)
    }

    /// 按键取值。
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries.get(key).map(String::as_str)
    }

    /// 是否包含键。
    #[must_use]
    pub fn contains_key(&self, key: &str) -> bool {
        self.entries.contains_key(key)
    }

    /// 键值对迭代（插入序）。
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.entries
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
    }

    /// 键值对数量。
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 是否为空。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl FromIterator<(String, String)> for StrMap {
    fn from_iter<T: IntoIterator<Item = (String, String)>>(iter: T) -> Self {
        Self {
            entries: iter.into_iter().collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Timestamp {
        text.parse()
            .unwrap_or_else(|error| panic!("{text} 应可解析: {error}"))
    }

    #[test]
    fn wire_format_matches_pydantic_bytes() {
        // 期望值逐字节来自 scripts/probe_parity_wire.py 的探针输出。
        assert_eq!(
            parse("2026-08-24T12:00:00.123456Z").to_wire_string(),
            "2026-08-24T12:00:00.123456Z"
        );
        assert_eq!(
            parse("2026-08-24T12:00:00Z").to_wire_string(),
            "2026-08-24T12:00:00Z"
        );
        // 1 位小数补齐为 6 位（pydantic 行为）。
        assert_eq!(
            parse("2026-08-24T12:00:00.1Z").to_wire_string(),
            "2026-08-24T12:00:00.100000Z"
        );
        // 全零小数折叠掉。
        assert_eq!(
            parse("2026-08-24T12:00:00.000Z").to_wire_string(),
            "2026-08-24T12:00:00Z"
        );
        // 7-9 位小数截断到微秒。
        assert_eq!(
            parse("2026-08-24T12:00:00.123456789Z").to_wire_string(),
            "2026-08-24T12:00:00.123456Z"
        );
        // 非 UTC 偏移原样保留。
        assert_eq!(
            parse("2026-08-24T05:00:00.987654-07:00").to_wire_string(),
            "2026-08-24T05:00:00.987654-07:00"
        );
        // 空格分隔符与小写 t/z 被 pydantic 接受，这里同样接受。
        assert_eq!(
            parse("2026-08-24 12:00:00Z").to_wire_string(),
            "2026-08-24T12:00:00Z"
        );
        assert_eq!(
            parse("2026-08-24t12:00:00z").to_wire_string(),
            "2026-08-24T12:00:00Z"
        );
    }

    #[test]
    fn isoformat_matches_python_stdlib_bytes() {
        // created_at 列格式：UTC 写 +00:00，微秒非零恒 6 位。
        assert_eq!(
            parse("2026-08-24T12:00:00.123456Z").isoformat(),
            "2026-08-24T12:00:00.123456+00:00"
        );
        assert_eq!(
            parse("2026-08-24T12:00:00Z").isoformat(),
            "2026-08-24T12:00:00+00:00"
        );
        assert_eq!(
            parse("2026-08-24T05:00:00.987654-07:00").isoformat(),
            "2026-08-24T05:00:00.987654-07:00"
        );
    }

    #[test]
    fn equality_compares_instants_like_python() {
        // Python: datetime(2026,8,24,12,tz=UTC) == datetime(2026,8,24,5,tz=-07:00)
        let utc = parse("2026-08-24T12:00:00Z");
        let minus7 = parse("2026-08-24T05:00:00-07:00");
        assert_eq!(utc, minus7);
        assert_ne!(utc, parse("2026-08-24T12:00:01Z"));
    }

    #[test]
    fn leap_seconds_are_rejected_like_python() {
        assert!("2026-08-24T12:00:60Z".parse::<Timestamp>().is_err());
    }

    #[test]
    fn serde_roundtrips_through_wire_string() {
        let value = parse("2026-08-24T12:00:00.123456Z");
        let json = serde_json::to_string(&value).unwrap_or_default();
        assert_eq!(json, "\"2026-08-24T12:00:00.123456Z\"");
        let back: Timestamp = serde_json::from_str(&json)
            .unwrap_or_else(|error| panic!("合法 wire 值必须可反序列化: {error}"));
        assert_eq!(back, value);
    }

    #[test]
    fn new_id_uses_prefix_and_twelve_hex_chars() {
        let id = new_id("mission");
        assert!(id.starts_with("mission_"), "前缀必须保留: {id}");
        assert_eq!(id.len(), "mission_".len() + 12);
        assert!(
            id["mission_".len()..]
                .chars()
                .all(|c| c.is_ascii_hexdigit()),
            "后缀必须是十六进制: {id}"
        );
    }

    #[test]
    fn strmap_preserves_insertion_order_like_python_dict() {
        let mut map = StrMap::new();
        map.insert("zz".to_string(), "last".to_string());
        map.insert("aa".to_string(), "first".to_string());
        let json = serde_json::to_string(&map).unwrap_or_default();
        assert_eq!(json, r#"{"zz":"last","aa":"first"}"#);
    }

    #[test]
    fn strmap_overwrite_keeps_original_position() {
        let mut map = StrMap::new();
        map.insert("a".to_string(), "1".to_string());
        map.insert("b".to_string(), "2".to_string());
        assert_eq!(
            map.insert("a".to_string(), "3".to_string()),
            Some("1".to_string())
        );
        let json = serde_json::to_string(&map).unwrap_or_default();
        // Python: {'a': 1, 'b': 2}; d['a'] = 3 → 顺序不变。
        assert_eq!(json, r#"{"a":"3","b":"2"}"#);
    }

    #[test]
    fn strmap_equality_is_order_insensitive_like_python_dict() {
        let mut left = StrMap::new();
        left.insert("a".to_string(), "1".to_string());
        left.insert("b".to_string(), "2".to_string());
        let mut right = StrMap::new();
        right.insert("b".to_string(), "2".to_string());
        right.insert("a".to_string(), "1".to_string());
        assert_eq!(left, right);
        right.insert("c".to_string(), "3".to_string());
        assert_ne!(left, right);
    }

    #[test]
    fn strmap_deserialize_rejects_non_string_values() {
        // pydantic 对 dict[str, str] 的 int 值同样报错。
        let result: Result<StrMap, _> = serde_json::from_str(r#"{"url":123}"#);
        assert!(result.is_err(), "非字符串值必须被拒绝");
        let ok: StrMap = serde_json::from_str(r#"{"url":"https://t"}"#)
            .unwrap_or_else(|error| panic!("字符串值必须可解析: {error}"));
        assert_eq!(ok.get("url"), Some("https://t"));
    }
}
