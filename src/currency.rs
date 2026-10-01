/// Currencies suggested when typing a code, roughly by how likely they are to be used.
pub const COMMON: &[&str] = &[
    "PLN", "EUR", "USD", "GBP", "CHF", "CZK", "SEK", "NOK", "DKK", "HUF", "RON", "BGN", "UAH", "JPY", "CNY", "CAD",
    "AUD", "NZD", "TRY", "ISK",
];

/// Currencies whose minor unit isn't a hundredth, with their number of decimal places.
const DECIMALS: &[(&str, u32)] = &[
    ("BHD", 3),
    ("CLP", 0),
    ("IQD", 3),
    ("ISK", 0),
    ("JOD", 3),
    ("JPY", 0),
    ("KRW", 0),
    ("KWD", 3),
    ("LYD", 3),
    ("OMR", 3),
    ("PYG", 0),
    ("TND", 3),
    ("UGX", 0),
    ("VND", 0),
];

/// An ISO 4217 currency code such as `PLN`. Any three letters are accepted, so currencies
/// the app doesn't know about can be used too.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Currency([u8; 3]);

impl Currency {
    /// Accepts three ASCII letters in any case, ignoring surrounding whitespace.
    pub fn parse(input: &str) -> Option<Self> {
        let code: [u8; 3] = input.trim().as_bytes().try_into().ok()?;
        code.iter().all(u8::is_ascii_alphabetic).then(|| Self(code.map(|letter| letter.to_ascii_uppercase())))
    }

    /// The upper case code, e.g. `PLN`.
    pub fn code(&self) -> &str {
        std::str::from_utf8(&self.0).expect("currency codes are ASCII")
    }

    /// Number of decimal places of amounts, e.g. 2 for cents. Unknown currencies get 2.
    pub fn decimals(&self) -> u32 {
        DECIMALS.iter().find(|(code, _)| *code == self.code()).map_or(2, |&(_, decimals)| decimals)
    }

    /// Common currencies whose code starts with the typed text, ignoring case and surrounding
    /// whitespace.
    pub fn suggest(query: &str) -> Vec<&'static str> {
        let query = query.trim().to_ascii_uppercase();
        COMMON.iter().copied().filter(|code| code.starts_with(&query)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_codes_into_upper_case() {
        assert_eq!(Currency::parse("PLN").unwrap().code(), "PLN");
        assert_eq!(Currency::parse(" eur ").unwrap().code(), "EUR");
        assert_eq!(Currency::parse("uSd").unwrap().code(), "USD");
    }

    #[test]
    fn rejects_anything_but_three_letters() {
        for input in ["", "PL", "PLNN", "P1N", "PL N", "ZŁ", "zł.", "€", "ÄÖÜ"] {
            assert_eq!(Currency::parse(input), None, "input: {input:?}");
        }
    }

    #[test]
    fn knows_decimal_places() {
        let decimals = |code| Currency::parse(code).unwrap().decimals();
        assert_eq!(decimals("PLN"), 2);
        assert_eq!(decimals("EUR"), 2);
        assert_eq!(decimals("JPY"), 0);
        assert_eq!(decimals("krw"), 0);
        assert_eq!(decimals("KWD"), 3);
        assert_eq!(decimals("XYZ"), 2, "unknown currencies have cents");
    }

    #[test]
    fn common_currencies_are_valid() {
        for code in COMMON {
            assert_eq!(Currency::parse(code).unwrap().code(), *code);
        }
    }

    #[test]
    fn suggests_common_currencies_by_prefix() {
        assert_eq!(Currency::suggest("").len(), COMMON.len());
        assert_eq!(Currency::suggest(" c"), ["CHF", "CZK", "CNY", "CAD"]);
        assert_eq!(Currency::suggest("pl"), ["PLN"]);
        assert!(Currency::suggest("XYZ").is_empty());
    }
}
