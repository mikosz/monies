use chrono::{NaiveDate, TimeDelta};

#[cfg(windows)]
mod windows;

const ISO_PATTERN: &str = "%Y-%m-%d";

/// How dates are shown and entered. ISO `YYYY-MM-DD` input is always accepted as well.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DateFormat {
    /// chrono strftime pattern.
    pattern: String,
    /// Human-readable form of the pattern, e.g. `D.MM.YYYY`.
    placeholder: String,
}

impl DateFormat {
    pub fn iso() -> Self {
        Self {
            pattern: ISO_PATTERN.to_owned(),
            placeholder: "YYYY-MM-DD".to_owned(),
        }
    }

    /// The user's short date format where it can be determined (currently Windows only),
    /// otherwise ISO.
    pub fn system() -> Self {
        #[cfg(windows)]
        if let Some(format) = windows::short_date_pattern().and_then(|p| Self::from_windows_pattern(&p)) {
            return format;
        }
        Self::iso()
    }

    /// Converts a Windows date pattern such as `d.MM.yyyy`. Returns `None` for patterns that
    /// can't be both formatted and parsed numerically (month or day names, eras, missing parts).
    /// Kept platform-independent so it's tested everywhere.
    #[cfg_attr(not(windows), allow(dead_code))]
    fn from_windows_pattern(windows_pattern: &str) -> Option<Self> {
        fn push_literal(pattern: &mut String, placeholder: &mut String, c: char) {
            if c == '%' {
                pattern.push_str("%%");
            } else {
                pattern.push(c);
            }
            placeholder.push(c);
        }

        let mut pattern = String::new();
        let mut placeholder = String::new();
        let (mut has_day, mut has_month, mut has_year) = (false, false, false);

        let mut chars = windows_pattern.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                // Quoted literal; '' outside quotes is a literal quote.
                '\'' => {
                    if chars.next_if_eq(&'\'').is_some() {
                        pattern.push('\'');
                        placeholder.push('\'');
                        continue;
                    }
                    for c in chars.by_ref().take_while(|&c| c != '\'') {
                        push_literal(&mut pattern, &mut placeholder, c);
                    }
                }
                'd' | 'M' | 'y' => {
                    let mut count = 1;
                    while chars.next_if_eq(&c).is_some() {
                        count += 1;
                    }
                    let (item, shown) = match (c, count) {
                        ('d', 1) => ("%-d", "D"),
                        ('d', 2) => ("%d", "DD"),
                        ('M', 1) => ("%-m", "M"),
                        ('M', 2) => ("%m", "MM"),
                        ('y', 1 | 2) => ("%y", "YY"),
                        ('y', 4 | 5) => ("%Y", "YYYY"),
                        _ => return None,
                    };
                    match c {
                        'd' => has_day = true,
                        'M' => has_month = true,
                        _ => has_year = true,
                    }
                    pattern.push_str(item);
                    placeholder.push_str(shown);
                }
                c if c.is_ascii_alphabetic() => return None,
                c => push_literal(&mut pattern, &mut placeholder, c),
            }
        }

        (has_day && has_month && has_year).then_some(Self { pattern, placeholder })
    }

    pub fn placeholder(&self) -> &str {
        &self.placeholder
    }

    pub fn format(&self, date: NaiveDate) -> String {
        date.format(&self.pattern).to_string()
    }

    /// Parses `text` in this format or, failing that, as ISO. Surrounding whitespace is ignored.
    pub fn parse(&self, text: &str) -> Option<NaiveDate> {
        let text = text.trim();
        NaiveDate::parse_from_str(text, &self.pattern)
            .or_else(|_| NaiveDate::parse_from_str(text, ISO_PATTERN))
            .ok()
    }

    /// Shifts the date in `text` by `days` (negative moves back). Empty text counts as `today`.
    /// Returns `None` when the text isn't a valid date or the result is out of range.
    pub fn shift(&self, text: &str, days: i64, today: NaiveDate) -> Option<String> {
        let date = if text.trim().is_empty() { today } else { self.parse(text)? };
        let shifted = date.checked_add_signed(TimeDelta::try_days(days)?)?;
        Some(self.format(shifted))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).unwrap()
    }

    fn windows(pattern: &str) -> DateFormat {
        DateFormat::from_windows_pattern(pattern).unwrap()
    }

    #[test]
    fn converts_windows_patterns() {
        let polish = windows("d.MM.yyyy");
        assert_eq!(polish.placeholder(), "D.MM.YYYY");
        assert_eq!(polish.format(date(2026, 10, 1)), "1.10.2026");

        let us = windows("M/d/yyyy");
        assert_eq!(us.placeholder(), "M/D/YYYY");
        assert_eq!(us.format(date(2026, 9, 5)), "9/5/2026");

        let british = windows("dd/MM/yy");
        assert_eq!(british.format(date(2026, 9, 5)), "05/09/26");
    }

    #[test]
    fn keeps_quoted_literals() {
        let format = windows("yyyy'r. 'MM-dd");
        assert_eq!(format.placeholder(), "YYYYr. MM-DD");
        assert_eq!(format.format(date(2026, 10, 1)), "2026r. 10-01");
        assert_eq!(format.parse("2026r. 10-01"), Some(date(2026, 10, 1)));
    }

    #[test]
    fn rejects_unsupported_windows_patterns() {
        for pattern in ["d MMM yyyy", "dddd, d.MM.yyyy", "d.MM.yyyy g", "MM.yyyy", "d.MM.yyy", ""] {
            assert_eq!(DateFormat::from_windows_pattern(pattern), None, "pattern: {pattern:?}");
        }
    }

    #[test]
    fn parses_with_or_without_leading_zeros() {
        let polish = windows("d.MM.yyyy");
        assert_eq!(polish.parse(" 1.10.2026 "), Some(date(2026, 10, 1)));
        assert_eq!(polish.parse("01.10.2026"), Some(date(2026, 10, 1)));
        assert_eq!(polish.parse("1.9.2026"), Some(date(2026, 9, 1)));
    }

    #[test]
    fn parses_two_digit_years() {
        assert_eq!(windows("dd/MM/yy").parse("05/09/26"), Some(date(2026, 9, 5)));
    }

    #[test]
    fn falls_back_to_iso() {
        assert_eq!(windows("d.MM.yyyy").parse("2026-10-01"), Some(date(2026, 10, 1)));
        assert_eq!(DateFormat::iso().parse("2026-10-01"), Some(date(2026, 10, 1)));
    }

    #[test]
    fn rejects_invalid_dates() {
        let polish = windows("d.MM.yyyy");
        for text in ["", "30.02.2026", "10/1/2026", "2026-10", "1.10"] {
            assert_eq!(polish.parse(text), None, "text: {text:?}");
        }
    }

    #[test]
    fn shifts_dates() {
        let format = windows("d.MM.yyyy");
        let today = date(2026, 10, 1);
        assert_eq!(format.shift("30.09.2026", 1, today).as_deref(), Some("1.10.2026"));
        assert_eq!(format.shift("31.12.2026", 1, today).as_deref(), Some("1.01.2027"));
        assert_eq!(format.shift("1.03.2028", -1, today).as_deref(), Some("29.02.2028"));
        assert_eq!(format.shift("1.03.2027", -1, today).as_deref(), Some("28.02.2027"));
        // ISO input is shifted and shown in the configured format.
        assert_eq!(format.shift("2026-10-01", -1, today).as_deref(), Some("30.09.2026"));
    }

    #[test]
    fn shifts_from_today_when_empty() {
        let format = DateFormat::iso();
        let today = date(2026, 10, 1);
        assert_eq!(format.shift("", -1, today).as_deref(), Some("2026-09-30"));
        assert_eq!(format.shift("  ", 1, today).as_deref(), Some("2026-10-02"));
    }

    #[test]
    fn does_not_shift_invalid_dates() {
        let format = DateFormat::iso();
        let today = date(2026, 10, 1);
        assert_eq!(format.shift("2026-10", 1, today), None);
        assert_eq!(format.shift("2026-02-30", 1, today), None);
        assert_eq!(format.shift(&format.format(NaiveDate::MAX), 1, today), None);
    }
}
