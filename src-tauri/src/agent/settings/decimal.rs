//! Python's `decimal.Decimal`, as far as the Agent's money and percentage
//! settings use it (the web's `settings._to_decimal` and `_serialise`).
//!
//! The settings screen shows these values exactly as the web stores them:
//! `"500000.00"`, `"5"`, `"5.50"`, `"1E+3"`. A float would print `5.5` and
//! lose the trailing zero the trader typed, and the bounds messages quote
//! the value, so parsing, comparison, rounding to paise and printing follow
//! Python's rules: the coefficient keeps its digits, the exponent decides
//! where the point goes, `ROUND_HALF_UP` to two places for money.

use std::cmp::Ordering;

/// Python's default context precision: a quantised money value may not
/// carry more significant digits (Python raises `InvalidOperation`).
const PRECISION: usize = 28;

/// A parsed decimal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PyDecimal {
    Finite(Finite),
    /// `Infinity`, `-Infinity`, `NaN`: parsed so the setting can say "must
    /// be a finite number", as the web does.
    Special(String),
}

/// A finite value: `(-1)^negative * digits * 10^exponent`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finite {
    negative: bool,
    /// The coefficient, without leading zeros (`"0"` for zero).
    digits: String,
    exponent: i64,
}

impl PyDecimal {
    /// `Decimal(text)` after the caller's strip: `None` where Python raises
    /// `InvalidOperation`. Underscores between digits are accepted, as
    /// Python's constructor accepts them.
    pub fn parse(text: &str) -> Option<PyDecimal> {
        let (negative, rest) = match text.as_bytes().first() {
            Some(b'-') => (true, &text[1..]),
            Some(b'+') => (false, &text[1..]),
            _ => (false, text),
        };
        let lower = rest.to_ascii_lowercase();
        if matches!(lower.as_str(), "inf" | "infinity") {
            return Some(PyDecimal::Special(
                if negative { "-Infinity" } else { "Infinity" }.into(),
            ));
        }
        if let Some(payload) = lower
            .strip_prefix("nan")
            .or_else(|| lower.strip_prefix("snan"))
        {
            return payload
                .chars()
                .all(|c| c.is_ascii_digit())
                .then(|| PyDecimal::Special("NaN".into()));
        }
        let (mantissa, exp_part) = match rest.find(['e', 'E']) {
            Some(i) => (&rest[..i], Some(&rest[i + 1..])),
            None => (rest, None),
        };
        let (int_part, frac_part) = match mantissa.split_once('.') {
            Some((i, f)) => (i, f),
            None => (mantissa, ""),
        };
        let int_digits = digits_of(int_part)?;
        let frac_digits = digits_of(frac_part)?;
        if int_digits.is_empty() && frac_digits.is_empty() {
            return None;
        }
        let mut exponent: i64 = match exp_part {
            Some(e) => parse_exponent(e)?,
            None => 0,
        };
        exponent = exponent.checked_sub(i64::try_from(frac_digits.len()).ok()?)?;
        let joined = format!("{}{}", int_digits, frac_digits);
        let trimmed = joined.trim_start_matches('0');
        Some(PyDecimal::Finite(Finite {
            negative,
            digits: if trimmed.is_empty() { "0" } else { trimmed }.to_string(),
            exponent,
        }))
    }
}

/// Digits with single underscores between them (PEP 515); `""` for none.
fn digits_of(part: &str) -> Option<String> {
    if part.is_empty() {
        return Some(String::new());
    }
    if part.starts_with('_') || part.ends_with('_') || part.contains("__") {
        return None;
    }
    let clean: String = part.chars().filter(|c| *c != '_').collect();
    clean.chars().all(|c| c.is_ascii_digit()).then_some(clean)
}

fn parse_exponent(text: &str) -> Option<i64> {
    let (negative, rest) = match text.as_bytes().first() {
        Some(b'-') => (true, &text[1..]),
        Some(b'+') => (false, &text[1..]),
        _ => (false, text),
    };
    let digits = digits_of(rest)?;
    if digits.is_empty() {
        return None;
    }
    let value: i64 = digits.parse().ok()?;
    Some(if negative { -value } else { value })
}

impl Finite {
    /// A whole number (the bounds `0` and `100`).
    pub fn integer(value: u64) -> Finite {
        Finite {
            negative: false,
            digits: value.to_string(),
            exponent: 0,
        }
    }

    fn is_zero(&self) -> bool {
        self.digits == "0"
    }

    /// The exponent of the leading digit (Python's `adjusted()`).
    fn adjusted(&self) -> i64 {
        self.exponent + self.digits.len() as i64 - 1
    }

    /// Numeric comparison (Python's `<`, `>`).
    pub fn compare(&self, other: &Finite) -> Ordering {
        match (self.is_zero(), other.is_zero()) {
            (true, true) => return Ordering::Equal,
            (true, false) => {
                return if other.negative {
                    Ordering::Greater
                } else {
                    Ordering::Less
                }
            }
            (false, true) => {
                return if self.negative {
                    Ordering::Less
                } else {
                    Ordering::Greater
                }
            }
            _ => {}
        }
        if self.negative != other.negative {
            return if self.negative {
                Ordering::Less
            } else {
                Ordering::Greater
            };
        }
        let magnitude = self
            .adjusted()
            .cmp(&other.adjusted())
            .then_with(|| compare_digits(&self.digits, &other.digits));
        if self.negative {
            magnitude.reverse()
        } else {
            magnitude
        }
    }

    /// `quantize(Decimal("0.01"), ROUND_HALF_UP)`: `None` where the result
    /// needs more digits than Python's context precision allows.
    pub fn quantize_cents(&self) -> Option<Finite> {
        const TARGET: i64 = -2;
        let digits = if self.exponent >= TARGET {
            let pad = usize::try_from(self.exponent - TARGET).ok()?;
            if pad > PRECISION {
                return None;
            }
            format!("{}{}", self.digits, "0".repeat(pad))
        } else {
            let drop = usize::try_from(TARGET - self.exponent).ok()?;
            round_half_up(&self.digits, drop)
        };
        let trimmed = digits.trim_start_matches('0');
        let digits = if trimmed.is_empty() { "0" } else { trimmed }.to_string();
        if digits.len() > PRECISION {
            return None;
        }
        Some(Finite {
            negative: self.negative,
            digits,
            exponent: TARGET,
        })
    }
}

/// Equal-magnitude coefficients compared digit by digit (shorter padded with
/// zeros on the right).
fn compare_digits(a: &str, b: &str) -> Ordering {
    let width = a.len().max(b.len());
    let pad = |s: &str| format!("{:0<width$}", s, width = width);
    pad(a).cmp(&pad(b))
}

/// Drop the last `drop` digits, rounding half up (away from zero).
fn round_half_up(digits: &str, drop: usize) -> String {
    if drop > digits.len() {
        return "0".into();
    }
    let keep = &digits[..digits.len() - drop];
    let first_dropped = digits.as_bytes().get(digits.len() - drop).copied();
    let mut out: Vec<u8> = if keep.is_empty() {
        vec![b'0']
    } else {
        keep.as_bytes().to_vec()
    };
    if first_dropped.is_some_and(|d| d >= b'5') {
        let mut i = out.len();
        loop {
            if i == 0 {
                out.insert(0, b'1');
                break;
            }
            i -= 1;
            if out[i] == b'9' {
                out[i] = b'0';
            } else {
                out[i] += 1;
                break;
            }
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| "0".into())
}

impl std::fmt::Display for Finite {
    /// Python's `Decimal.__str__` (scientific notation when the exponent is
    /// positive or the value is very small).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let sign = if self.negative { "-" } else { "" };
        let len = self.digits.len() as i64;
        let left = self.exponent + len;
        let dot = if self.exponent <= 0 && left > -6 {
            left
        } else {
            1
        };
        let (int_part, frac_part) = if dot <= 0 {
            (
                "0".to_string(),
                format!(".{}{}", "0".repeat((-dot) as usize), self.digits),
            )
        } else if dot >= len {
            (
                format!("{}{}", self.digits, "0".repeat((dot - len) as usize)),
                String::new(),
            )
        } else {
            (
                self.digits[..dot as usize].to_string(),
                format!(".{}", &self.digits[dot as usize..]),
            )
        };
        let exp = if left == dot {
            String::new()
        } else {
            format!("E{:+}", left - dot)
        };
        write!(f, "{}{}{}{}", sign, int_part, frac_part, exp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fin(s: &str) -> Finite {
        match PyDecimal::parse(s) {
            Some(PyDecimal::Finite(f)) => f,
            other => panic!("{} parsed as {:?}", s, other),
        }
    }

    /// The strings Python prints for the same inputs.
    #[test]
    fn prints_as_python_does() {
        for (input, shown) in [
            ("5", "5"),
            ("5.50", "5.50"),
            ("500000.00", "500000.00"),
            ("0.000", "0.000"),
            ("-0", "-0"),
            ("007", "7"),
            ("1e3", "1E+3"),
            ("1.5e-3", "0.0015"),
            ("1E-7", "1E-7"),
            ("123.456e2", "12345.6"),
            ("1_000.5", "1000.5"),
            (".5", "0.5"),
            ("5.", "5"),
            ("+2", "2"),
        ] {
            assert_eq!(fin(input).to_string(), shown, "{}", input);
        }
    }

    #[test]
    fn refuses_what_python_refuses() {
        for bad in [
            "", ".", "abc", "1__0", "_1", "1_", "1e", "1e+", "--1", "1.2.3", "0x10",
        ] {
            assert!(PyDecimal::parse(bad).is_none(), "{}", bad);
        }
        for special in ["NaN", "nan", "inf", "-Infinity", "sNaN"] {
            assert!(matches!(
                PyDecimal::parse(special),
                Some(PyDecimal::Special(_))
            ));
        }
    }

    #[test]
    fn rounds_money_half_up_to_paise() {
        for (input, shown) in [
            ("1.005", "1.01"),
            ("1.004", "1.00"),
            ("-1.005", "-1.01"),
            ("9.995", "10.00"),
            ("500000", "500000.00"),
            ("1e3", "1000.00"),
            ("0.001", "0.00"),
        ] {
            assert_eq!(
                fin(input).quantize_cents().unwrap().to_string(),
                shown,
                "{}",
                input
            );
        }
        assert!(fin("1e40").quantize_cents().is_none());
    }

    #[test]
    fn compares_numerically() {
        let hundred = Finite::integer(100);
        assert_eq!(fin("100.0").compare(&hundred), Ordering::Equal);
        assert_eq!(fin("1E+2").compare(&hundred), Ordering::Equal);
        assert_eq!(fin("100.01").compare(&hundred), Ordering::Greater);
        assert_eq!(fin("99.999").compare(&hundred), Ordering::Less);
        assert_eq!(fin("-0.01").compare(&Finite::integer(0)), Ordering::Less);
        assert_eq!(fin("-0").compare(&Finite::integer(0)), Ordering::Equal);
        assert_eq!(fin("-5").compare(&fin("-3")), Ordering::Less);
    }
}
