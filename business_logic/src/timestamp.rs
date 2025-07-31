use arrayvec::ArrayString;
use core::fmt::Write;

// TODO: implement Format for Timestamp

/// Represents a timestamp in seconds since the epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timestamp {
    pub seconds: u32,
}

impl Timestamp {
    /// Create an an ISO 8601 Duration string.
    pub fn create_iso8601_str(&self) -> ArrayString<32> {
        let (days, hours, minutes, remaining_seconds) = self.to_dhms();
        let mut result = ArrayString::<32>::new();
        if days > 0 {
            write!(&mut result, "P{}D", days).expect("can't write");
        } else {
            result.push_str("P0D");
        }
        if hours > 0 || minutes > 0 || remaining_seconds > 0 {
            write!(&mut result, "T{}H{}M{}S", hours, minutes, remaining_seconds).expect("can't write");
        } else {
            result.push_str("T0S");
        }
        result
    }

    /// Converts seconds since the epoch to days, hours, minutes, and seconds.
    pub fn to_dhms(&self) -> (u32, u32, u32, u32) {
        let days = self.seconds / 86400;
        let seconds_of_day = self.seconds - days * 86400;
        let hours = seconds_of_day / 3600;
        let remaining_seconds = seconds_of_day - hours * 3600;
        let minutes = remaining_seconds / 60;
        let remaining_seconds = remaining_seconds - minutes * 60;
        (days, hours, minutes, remaining_seconds)
    }

    /// Parses an ISO 8601 Duration string and returns the number of days, hours, minutes, and seconds.
    pub fn parse_duration(input: &str) -> Option<(u32, u32, u32, u32)> {
        if input.starts_with("P0DT0S") {
            return Some((0, 0, 0, 0));
        }
        let input = input.strip_prefix('P')?;
        let (days_str, rest) = input.split_once("DT")?;
        let (hours_str, rest) = rest.split_once('H')?;
        let (minutes_str, rest) = rest.split_once('M')?;
        let seconds_str = rest.strip_suffix('S')?;
        Some((
            days_str.parse().ok()?,
            hours_str.parse().ok()?,
            minutes_str.parse().ok()?,
            seconds_str.parse().ok()?
        ))
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_to_dhms() {
        let ts = Timestamp { seconds: 93784 };
        assert_eq!(ts.to_dhms(), (1, 2, 3, 4));
    }

    #[test]
    fn test_create_iso8601_str() {
        let ts = Timestamp { seconds: 93784 };
        assert_eq!(ts.create_iso8601_str().as_str(), "P1DT2H3M4S");
        let ts = Timestamp { seconds: 0 };
        assert_eq!(ts.create_iso8601_str().as_str(), "P0DT0S");
    }

    #[test]
    fn test_parse_duration() {
        let parsed = Timestamp::parse_duration("P1DT2H3M4S");
        assert_eq!(parsed, Some((1, 2, 3, 4)));
        let parsed = Timestamp::parse_duration("P0DT0S");
        assert_eq!(parsed, Some((0, 0, 0, 0)));
        let parsed = Timestamp::parse_duration("P2DT3H4M5");
        assert_eq!(parsed, None); // Invalid format, missing seconds.
    }
}
