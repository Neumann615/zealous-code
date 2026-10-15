//! 时间工具:Unix 毫秒 → RFC3339 UTC 字符串。
//! 不引入 chrono,账本时间戳格式由这里统一保证(spec 20 信封 `ts` 字段)。

/// civil-from-days 算法(Hinnant),把 Unix 毫秒格式化为 `YYYY-MM-DDTHH:MM:SS.mmmZ`。
pub fn rfc3339_from_unix_millis(millis: u64) -> String {
    let secs = (millis / 1000) as i64;
    let ms = (millis % 1000) as u32;
    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400);

    // civil-from-days:以 1970-03-01 为纪元的偏移法
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    let (hh, mm, ss) = (
        secs_of_day / 3600,
        secs_of_day % 3600 / 60,
        secs_of_day % 60,
    );
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}.{ms:03}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_timestamps() {
        assert_eq!(rfc3339_from_unix_millis(0), "1970-01-01T00:00:00.000Z");
        // 2026-10-15T00:00:00Z = 1792022400 * 1000
        assert_eq!(
            rfc3339_from_unix_millis(1_792_022_400_000),
            "2026-10-15T00:00:00.000Z"
        );
        // 闰年边界:2024-02-29T23:59:59.999Z = 1709251199 * 1000 + 999
        assert_eq!(
            rfc3339_from_unix_millis(1_709_251_199_999),
            "2024-02-29T23:59:59.999Z"
        );
    }
}
