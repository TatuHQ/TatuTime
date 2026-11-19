pub(crate) use jiff::ZonedDateTime;
pub(crate) use jiff::{
    SignedDuration, Timestamp, ToSpan,
    civil::{Date as CivilDate, DateTime as CivilDateTime, Weekday},
    tz::TimeZone,
};

pub(crate) trait CivilDateExt {
    fn checked_add_days(self, days: u64) -> Option<CivilDate>;
    fn month0(self) -> u32;
    fn pred_opt(self) -> Option<CivilDate>;
    fn succ_opt(self) -> Option<CivilDate>;
}

impl CivilDateExt for CivilDate {
    fn checked_add_days(self, days: u64) -> Option<CivilDate> {
        let days = i64::try_from(days).ok()?;
        self.checked_add(jiff::Span::new().days(days)).ok()
    }

    fn month0(self) -> u32 {
        u32::try_from(self.month() - 1).expect("month is in 1..=12")
    }

    fn pred_opt(self) -> Option<CivilDate> {
        self.checked_sub(jiff::Span::new().days(1)).ok()
    }

    fn succ_opt(self) -> Option<CivilDate> {
        self.checked_add(jiff::Span::new().days(1)).ok()
    }
}

pub(crate) trait CivilSpanExt {
    fn num_days(self) -> i64;
}

impl CivilSpanExt for jiff::Span {
    fn num_days(self) -> i64 {
        i64::from(self.get_days())
    }
}

pub(crate) fn utc_timestamp(
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
) -> Timestamp {
    CivilDate::new(
        i16::try_from(year).expect("year is in Jiff's supported range"),
        i8::try_from(month).expect("month is in 1..=12"),
        i8::try_from(day).expect("day is in 1..=31"),
    )
    .unwrap()
    .at(
        i8::try_from(hour).expect("hour is in 0..=23"),
        i8::try_from(minute).expect("minute is in 0..=59"),
        i8::try_from(second).expect("second is in 0..=59"),
        0,
    )
    .in_tz("UTC")
    .unwrap()
    .timestamp()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_serde_accepts_offsets_and_normalizes_to_utc() {
        let timestamp: Timestamp =
            serde_json::from_str("\"2026-01-02T03:04:05+02:00\"").expect("timestamp");

        assert_eq!(timestamp.to_string(), "2026-01-02T01:04:05Z");
    }
}
