//! 实体 ID:ULID 形态(48bit 毫秒时间 + 80bit 随机性),零外部依赖。
//!
//! 随机性来源:进程启动时的纳秒时钟 + 静态变量地址(ASLR)混合出一个进程种子,
//! 进程内由单调计数器保证不重复;跨进程靠毫秒时间戳区分(spec 20:无需中心分配器)。

use serde::{de, Deserialize, Deserializer, Serialize, Serializer};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

fn process_seed() -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    // 静态量地址带 ASLR 熵;hash 混合避免规律性
    let addr = &nanos as *const u64 as u64;
    let mut x = nanos ^ addr.rotate_left(32);
    x ^= x >> 33;
    x = x.wrapping_mul(0xff51afd7ed558ccd);
    x ^= x >> 33;
    x
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn seed_base() -> u64 {
    // 懒初始化进程种子;OnceLock 保证只算一次
    static BASE: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *BASE.get_or_init(process_seed)
}

fn next_random() -> u64 {
    let base = seed_base();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut x = base ^ n.wrapping_mul(0x9e37_79b9_7f4a_7c15);
    x ^= x >> 30;
    x = x.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x ^= x >> 27;
    x
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn encode_ulid(time_ms: u64, rand_hi: u64, rand_lo: u64) -> String {
    // 48bit 时间 << 80bit 随机 = 128bit
    let mut value = ((time_ms as u128 & 0xFFFF_FFFF_FFFF) << 80)
        | ((rand_hi as u128 & 0xFFFF) << 64)
        | (rand_lo as u128);
    let mut out = [b'0'; 26];
    for i in (0..26).rev() {
        out[i] = CROCKFORD[(value & 0x1f) as usize];
        value >>= 5;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// ID 解析错误(spec 60:非法输入必须在触达文件系统前被拒绝)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdError {
    pub raw: String,
    pub reason: String,
}

impl std::fmt::Display for IdError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid id {:?}: {}", self.raw, self.reason)
    }
}

impl std::error::Error for IdError {}

/// ULID 尾段:26 位 Crockford Base32 字符
fn is_crockford_ulid_tail(s: &str) -> bool {
    s.len() == 26
        && s.bytes()
            .all(|b| CROCKFORD.contains(&b.to_ascii_uppercase()))
}

macro_rules! define_id {
    ($name:ident, $prefix:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(pub(crate) String);

        impl $name {
            pub fn generate() -> Self {
                let t = now_millis();
                let hi = next_random();
                let lo = next_random();
                Self(format!($prefix, encode_ulid(t, hi & 0xFFFF, lo)))
            }

            /// 唯一合法的外部构造入口:prefix + 26 位 Crockford ULID。
            /// 防路径穿越(spec 60):`../x`、任意字符串都在这里被拒绝。
            pub fn parse(s: &str) -> Result<Self, crate::ids::IdError> {
                // $prefix 是 format! 模板(如 "task_{}"),剥掉占位符得到纯前缀
                let plain: &str = $prefix.trim_end_matches("{}");
                let expected_len = plain.len() + 26;
                let reason = if !s.starts_with(plain) {
                    format!("must start with {plain:?}")
                } else if s.len() != expected_len {
                    format!("length must be {expected_len}")
                } else if !is_crockford_ulid_tail(&s[plain.len()..]) {
                    "tail must be 26 Crockford base32 chars".to_string()
                } else {
                    return Ok(Self(s.to_string()));
                };
                Err(crate::ids::IdError {
                    raw: s.to_string(),
                    reason,
                })
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let s = String::deserialize(d)?;
                Self::parse(&s).map_err(de::Error::custom)
            }
        }
    };
}

define_id!(TaskId, "task_{}");
define_id!(AgentRunId, "run_{}");
define_id!(ToolCallId, "call_{}");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_have_prefix_and_are_unique() {
        let a = TaskId::generate();
        let b = TaskId::generate();
        assert!(a.as_str().starts_with("task_"));
        assert_eq!(a.as_str().len(), "task_".len() + 26);
        assert_ne!(a, b);
    }

    #[test]
    fn id_survives_json_roundtrip() {
        let id = ToolCallId::generate();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, format!("\"{id}\""));
        assert_eq!(serde_json::from_str::<ToolCallId>(&json).unwrap(), id);
    }

    #[test]
    fn parse_rejects_path_traversal_and_garbage() {
        for bad in [
            "../evil",
            "task_../evil",
            "task_",
            "task_ABCDEFGHIJKLMNOPQRSTU111",   // 25 位
            "task_ABCDEFGHIJKLMNOPQRSTU11111", // 27 位
            "task_abcdefghijklmnopqrstuv1111", // 非法字符 小写 i/l/o/u 不在 CROCKFORD
            "run_ABCDEFGHIJKLMNOPQRSTUV1111",  // 前缀不符
            "",
        ] {
            let err = TaskId::parse(bad).expect_err(bad);
            assert!(!err.reason.is_empty());
        }
        // 合法:generate 产物必须能被 parse 接受
        let good = TaskId::generate();
        assert_eq!(TaskId::parse(good.as_str()).unwrap(), good);
    }
}
