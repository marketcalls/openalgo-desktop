//! Python's `int` as the Math Expression evaluator needs it: exact
//! addition, subtraction, multiplication, powers and floor modulo, and the
//! two conversions to `float` Python rounds correctly (`float(n)` and
//! `n / m`). Magnitudes are little-endian `u32` limbs.
//!
//! The web's evaluator caps every exponent at 100, but nested powers still
//! build large numbers (`((10**100)**100)**100` has a million digits), so a
//! value over [`MAX_BITS`] is refused with the sentence Python gives when
//! such a number reaches `float()`, which is where every such expression
//! ends.

use std::cmp::Ordering;

/// The largest integer the evaluator keeps (bits).
pub const MAX_BITS: u64 = 1 << 18;

/// Python's sentence for an integer no `float` can hold.
pub const TOO_LARGE: &str = "int too large to convert to float";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BigInt {
    neg: bool,
    mag: Vec<u32>,
}

fn trim(v: &mut Vec<u32>) {
    while v.last() == Some(&0) {
        v.pop();
    }
}

fn cmp_mag(a: &[u32], b: &[u32]) -> Ordering {
    if a.len() != b.len() {
        return a.len().cmp(&b.len());
    }
    for i in (0..a.len()).rev() {
        match a[i].cmp(&b[i]) {
            Ordering::Equal => continue,
            o => return o,
        }
    }
    Ordering::Equal
}

fn add_mag(a: &[u32], b: &[u32]) -> Vec<u32> {
    let (long, short) = if a.len() >= b.len() { (a, b) } else { (b, a) };
    let mut out = Vec::with_capacity(long.len() + 1);
    let mut carry = 0u64;
    for (i, x) in long.iter().enumerate() {
        let s = *x as u64 + short.get(i).copied().unwrap_or(0) as u64 + carry;
        out.push(s as u32);
        carry = s >> 32;
    }
    if carry > 0 {
        out.push(carry as u32);
    }
    out
}

/// `a - b` for `a >= b`.
fn sub_mag(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut out = Vec::with_capacity(a.len());
    let mut borrow = 0i64;
    for (i, x) in a.iter().enumerate() {
        let mut d = *x as i64 - b.get(i).copied().unwrap_or(0) as i64 - borrow;
        borrow = 0;
        if d < 0 {
            d += 1 << 32;
            borrow = 1;
        }
        out.push(d as u32);
    }
    trim(&mut out);
    out
}

fn mul_mag(a: &[u32], b: &[u32]) -> Vec<u32> {
    if a.is_empty() || b.is_empty() {
        return Vec::new();
    }
    let mut out = vec![0u32; a.len() + b.len()];
    for (i, x) in a.iter().enumerate() {
        let mut carry = 0u64;
        for (j, y) in b.iter().enumerate() {
            let t = *x as u64 * *y as u64 + out[i + j] as u64 + carry;
            out[i + j] = t as u32;
            carry = t >> 32;
        }
        let mut k = i + b.len();
        while carry > 0 {
            let t = out[k] as u64 + carry;
            out[k] = t as u32;
            carry = t >> 32;
            k += 1;
        }
    }
    trim(&mut out);
    out
}

fn bits_of(mag: &[u32]) -> u64 {
    match mag.last() {
        None => 0,
        Some(top) => (mag.len() as u64 - 1) * 32 + (32 - top.leading_zeros() as u64),
    }
}

fn shl_mag(a: &[u32], n: u64) -> Vec<u32> {
    if a.is_empty() {
        return Vec::new();
    }
    let words = (n / 32) as usize;
    let bits = (n % 32) as u32;
    let mut out = vec![0u32; words];
    if bits == 0 {
        out.extend_from_slice(a);
    } else {
        let mut carry = 0u32;
        for x in a {
            out.push((x << bits) | carry);
            carry = x >> (32 - bits);
        }
        if carry > 0 {
            out.push(carry);
        }
    }
    out
}

fn shr_mag(a: &[u32], n: u64) -> Vec<u32> {
    let words = (n / 32) as usize;
    if words >= a.len() {
        return Vec::new();
    }
    let bits = (n % 32) as u32;
    let src = &a[words..];
    let mut out = Vec::with_capacity(src.len());
    for i in 0..src.len() {
        let lo = src[i] >> bits;
        let hi = if bits == 0 {
            0
        } else {
            src.get(i + 1).map(|x| x << (32 - bits)).unwrap_or(0)
        };
        out.push(lo | hi);
    }
    trim(&mut out);
    out
}

/// Whether any of the low `n` bits of `a` is set.
fn low_bits_set(a: &[u32], n: u64) -> bool {
    let words = (n / 32) as usize;
    if a[..words.min(a.len())].iter().any(|x| *x != 0) {
        return true;
    }
    let bits = (n % 32) as u32;
    bits > 0 && a.get(words).is_some_and(|x| x & ((1u32 << bits) - 1) != 0)
}

fn divrem_small(a: &[u32], d: u32) -> (Vec<u32>, u32) {
    let mut q = vec![0u32; a.len()];
    let mut rem = 0u64;
    for i in (0..a.len()).rev() {
        let cur = (rem << 32) | a[i] as u64;
        q[i] = (cur / d as u64) as u32;
        rem = cur % d as u64;
    }
    trim(&mut q);
    (q, rem as u32)
}

/// Knuth's algorithm D: `(a / b, a % b)` of magnitudes, `b` non-empty.
fn divrem_mag(a: &[u32], b: &[u32]) -> (Vec<u32>, Vec<u32>) {
    if cmp_mag(a, b) == Ordering::Less {
        return (Vec::new(), a.to_vec());
    }
    if b.len() == 1 {
        let (q, r) = divrem_small(a, b[0]);
        let mut r = vec![r];
        trim(&mut r);
        return (q, r);
    }
    let shift = b[b.len() - 1].leading_zeros() as u64;
    let v = shl_mag(b, shift);
    let mut u = shl_mag(a, shift);
    if u.len() == a.len() {
        u.push(0);
    }
    let n = v.len();
    let m = u.len() - n;
    let mut q = vec![0u32; m];
    let vtop = v[n - 1] as u64;
    let vnext = v[n - 2] as u64;
    for j in (0..m).rev() {
        let num = ((u[j + n] as u64) << 32) | u[j + n - 1] as u64;
        let mut qhat = num / vtop;
        let mut rhat = num % vtop;
        while qhat >= 1 << 32 || qhat * vnext > ((rhat << 32) | u[j + n - 2] as u64) {
            qhat -= 1;
            rhat += vtop;
            if rhat >= 1 << 32 {
                break;
            }
        }
        let mut borrow = 0i64;
        let mut carry = 0u64;
        for i in 0..n {
            let p = qhat * v[i] as u64 + carry;
            carry = p >> 32;
            let t = u[i + j] as i64 - borrow - (p & 0xffff_ffff) as i64;
            u[i + j] = t as u32;
            borrow = if t < 0 { 1 } else { 0 };
        }
        let t = u[j + n] as i64 - borrow - carry as i64;
        u[j + n] = t as u32;
        if t < 0 {
            qhat -= 1;
            let mut c = 0u64;
            for i in 0..n {
                let s = u[i + j] as u64 + v[i] as u64 + c;
                u[i + j] = s as u32;
                c = s >> 32;
            }
            u[j + n] = u[j + n].wrapping_add(c as u32);
        }
        q[j] = qhat as u32;
    }
    trim(&mut q);
    u.truncate(n);
    let mut r = shr_mag(&u, shift);
    trim(&mut r);
    (q, r)
}

/// `mag * 2**exp` as the nearest `f64` (ties to even), `sticky` saying
/// whether non-zero bits below `mag` were dropped before; `None` when the
/// result is too large for a float.
fn round_to_f64(mag: &[u32], exp: i64, sticky: bool) -> Option<f64> {
    let nbits = bits_of(mag) as i64;
    if nbits == 0 {
        return Some(0.0);
    }
    // The value lies in [2**top, 2**(top + 1)).
    let top = nbits - 1 + exp;
    if top > 1023 {
        return None;
    }
    // The weight of the last bit a float keeps here (subnormals keep fewer).
    let unit = (top - 52).max(-1074);
    let drop = unit - exp;
    let k = if drop <= 0 {
        shl_mag(mag, (-drop) as u64)
    } else {
        let d = drop as u64;
        let mut k = shr_mag(mag, d);
        let half = bit(mag, d - 1);
        let rest = sticky || low_bits_set(mag, d - 1);
        let odd = k.first().is_some_and(|x| x & 1 == 1);
        if half && (rest || odd) {
            k = add_mag(&k, &[1]);
        }
        k
    };
    let v = ldexp(mag_to_u64(&k) as f64, unit);
    if v.is_infinite() {
        None
    } else {
        Some(v)
    }
}

fn bit(mag: &[u32], n: u64) -> bool {
    mag.get((n / 32) as usize)
        .is_some_and(|x| (x >> (n % 32)) & 1 == 1)
}

/// `x * 2**e`, exact while the result is representable.
fn ldexp(mut x: f64, mut e: i64) -> f64 {
    while e > 1000 {
        x *= 2f64.powi(1000);
        e -= 1000;
    }
    while e < -1000 {
        x *= 2f64.powi(-1000);
        e += 1000;
    }
    // Split so an intermediate never rounds before the final step.
    let half = e / 2;
    x * 2f64.powi(half as i32) * 2f64.powi((e - half) as i32)
}

impl BigInt {
    pub fn zero() -> Self {
        Self {
            neg: false,
            mag: Vec::new(),
        }
    }

    pub fn from_u64(v: u64) -> Self {
        let mut mag = vec![v as u32, (v >> 32) as u32];
        trim(&mut mag);
        Self { neg: false, mag }
    }

    pub fn from_i64(v: i64) -> Self {
        let mut b = Self::from_u64(v.unsigned_abs());
        b.neg = v < 0 && !b.mag.is_empty();
        b
    }

    fn from_parts(neg: bool, mut mag: Vec<u32>) -> Self {
        trim(&mut mag);
        let neg = neg && !mag.is_empty();
        Self { neg, mag }
    }

    /// Digits in `radix` (2, 8, 10, 16), underscores already removed.
    pub fn parse_radix(digits: &str, radix: u32) -> Option<Self> {
        let mut mag: Vec<u32> = Vec::new();
        for c in digits.chars() {
            let d = c.to_digit(radix)?;
            let mut carry = d as u64;
            for limb in mag.iter_mut() {
                let t = *limb as u64 * radix as u64 + carry;
                *limb = t as u32;
                carry = t >> 32;
            }
            if carry > 0 {
                mag.push(carry as u32);
            }
        }
        Some(Self::from_parts(false, mag))
    }

    /// The integer part of a finite float (`math.floor` hands its result
    /// here after flooring), exactly.
    pub fn from_f64_integral(f: f64) -> Self {
        let neg = f < 0.0;
        let a = f.abs();
        if a < 1.0 {
            return Self::zero();
        }
        let bits = a.to_bits();
        let exp = ((bits >> 52) & 0x7ff) as i64 - 1075;
        let mant = (bits & ((1u64 << 52) - 1)) | (1u64 << 52);
        let m = Self::from_u64(mant).mag;
        let mag = if exp >= 0 {
            shl_mag(&m, exp as u64)
        } else {
            shr_mag(&m, (-exp) as u64)
        };
        Self::from_parts(neg, mag)
    }

    pub fn is_zero(&self) -> bool {
        self.mag.is_empty()
    }

    pub fn is_negative(&self) -> bool {
        self.neg
    }

    pub fn bits(&self) -> u64 {
        bits_of(&self.mag)
    }

    /// The value as an `i64`, when it fits.
    pub fn to_i64(&self) -> Option<i64> {
        if self.mag.len() > 2 {
            return None;
        }
        let v = mag_to_u64(&self.mag);
        if self.neg {
            if v <= i64::MAX as u64 + 1 {
                Some((v as i128).wrapping_neg() as i64)
            } else {
                None
            }
        } else {
            i64::try_from(v).ok()
        }
    }

    pub fn neg(&self) -> Self {
        Self::from_parts(!self.neg, self.mag.clone())
    }

    pub fn abs_cmp_u64(&self, v: u64) -> Ordering {
        cmp_mag(&self.mag, &Self::from_u64(v).mag)
    }

    fn checked(self) -> Result<Self, String> {
        if self.bits() > MAX_BITS {
            Err(TOO_LARGE.into())
        } else {
            Ok(self)
        }
    }

    pub fn add(&self, o: &Self) -> Result<Self, String> {
        if self.neg == o.neg {
            return Self::from_parts(self.neg, add_mag(&self.mag, &o.mag)).checked();
        }
        match cmp_mag(&self.mag, &o.mag) {
            Ordering::Equal => Ok(Self::zero()),
            Ordering::Greater => Ok(Self::from_parts(self.neg, sub_mag(&self.mag, &o.mag))),
            Ordering::Less => Ok(Self::from_parts(o.neg, sub_mag(&o.mag, &self.mag))),
        }
    }

    pub fn sub(&self, o: &Self) -> Result<Self, String> {
        self.add(&o.neg())
    }

    pub fn mul(&self, o: &Self) -> Result<Self, String> {
        if self.bits() + o.bits() > MAX_BITS + 1 {
            return Err(TOO_LARGE.into());
        }
        Self::from_parts(self.neg != o.neg, mul_mag(&self.mag, &o.mag)).checked()
    }

    /// `self ** exp` for `0 <= exp <= 100` (the evaluator's cap).
    pub fn pow(&self, exp: u32) -> Result<Self, String> {
        if self.bits().saturating_sub(1) * exp as u64 > MAX_BITS {
            return Err(TOO_LARGE.into());
        }
        let mut result = Self::from_u64(1);
        let mut base = self.clone();
        let mut e = exp;
        while e > 0 {
            if e & 1 == 1 {
                result = result.mul(&base)?;
            }
            e >>= 1;
            if e > 0 {
                base = base.mul(&base)?;
            }
        }
        Ok(result)
    }

    /// Python's `self % o` (the sign of the divisor); `o` non-zero.
    pub fn py_mod(&self, o: &Self) -> Self {
        let (_, r) = divrem_mag(&self.mag, &o.mag);
        let r = Self::from_parts(self.neg, r);
        if !r.is_zero() && r.neg != o.neg {
            // r + o, with r and o of opposite signs.
            let sum = if cmp_mag(&o.mag, &r.mag) == Ordering::Greater {
                Self::from_parts(o.neg, sub_mag(&o.mag, &r.mag))
            } else {
                Self::from_parts(r.neg, sub_mag(&r.mag, &o.mag))
            };
            return sum;
        }
        r
    }

    /// Python's `float(self)`, correctly rounded; `Err` with Python's
    /// sentence when it overflows.
    pub fn to_f64(&self) -> Result<f64, String> {
        let v = round_to_f64(&self.mag, 0, false).ok_or_else(|| TOO_LARGE.to_string())?;
        Ok(if self.neg { -v } else { v })
    }

    /// Python's `self / o` (true division of two ints), correctly rounded;
    /// `o` non-zero.
    pub fn true_div(&self, o: &Self) -> Result<f64, String> {
        let neg = self.neg != o.neg;
        if self.is_zero() {
            return Ok(if neg { -0.0 } else { 0.0 });
        }
        let la = self.bits() as i64;
        let lb = o.bits() as i64;
        // Scale so the quotient carries at least 55 significant bits.
        let shift = 55 - (la - lb);
        let (num, den) = if shift >= 0 {
            (shl_mag(&self.mag, shift as u64), o.mag.clone())
        } else {
            (self.mag.clone(), shl_mag(&o.mag, (-shift) as u64))
        };
        let (q, r) = divrem_mag(&num, &den);
        let v = round_to_f64(&q, -shift, !r.is_empty())
            .ok_or_else(|| "integer division result too large for a float".to_string())?;
        Ok(if neg { -v } else { v })
    }

    /// The decimal text Python's `str(int)` writes.
    pub fn to_decimal(&self) -> String {
        if self.mag.is_empty() {
            return "0".into();
        }
        let mut chunks = Vec::new();
        let mut cur = self.mag.clone();
        while !cur.is_empty() {
            let (q, r) = divrem_small(&cur, 1_000_000_000);
            chunks.push(r);
            cur = q;
        }
        let mut s = String::new();
        if self.neg {
            s.push('-');
        }
        let mut iter = chunks.iter().rev();
        if let Some(first) = iter.next() {
            s.push_str(&first.to_string());
        }
        for c in iter {
            s.push_str(&format!("{:09}", c));
        }
        s
    }
}

fn mag_to_u64(mag: &[u32]) -> u64 {
    mag.iter()
        .take(2)
        .enumerate()
        .fold(0u64, |acc, (i, x)| acc | ((*x as u64) << (32 * i)))
}

impl PartialOrd for BigInt {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(match (self.neg, o.neg) {
            (false, true) => Ordering::Greater,
            (true, false) => Ordering::Less,
            (false, false) => cmp_mag(&self.mag, &o.mag),
            (true, true) => cmp_mag(&o.mag, &self.mag),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn int(s: &str) -> BigInt {
        let (neg, digits) = match s.strip_prefix('-') {
            Some(d) => (true, d),
            None => (false, s),
        };
        let v = BigInt::parse_radix(digits, 10).unwrap();
        if neg {
            v.neg()
        } else {
            v
        }
    }

    #[test]
    fn arithmetic_is_exact() {
        let big = int("123456789012345678901234567890");
        assert_eq!(big.to_decimal(), "123456789012345678901234567890");
        let sq = big.mul(&big).unwrap();
        assert_eq!(
            sq.to_decimal(),
            "15241578753238836750495351562536198787501905199875019052100"
        );
        assert_eq!(
            sq.sub(&big).unwrap().to_decimal(),
            "15241578753238836750495351562412741998489560520973784484210"
        );
        assert_eq!(
            int("10").pow(30).unwrap().to_decimal(),
            format!("1{}", "0".repeat(30))
        );
        assert_eq!(int("-7").py_mod(&int("3")).to_decimal(), "2");
        assert_eq!(int("7").py_mod(&int("-3")).to_decimal(), "-2");
        assert_eq!(int("-7").py_mod(&int("-3")).to_decimal(), "-1");
        let a = int("2").pow(70).unwrap();
        let m = int("2").pow(35).unwrap().add(&int("3")).unwrap();
        assert_eq!(a.py_mod(&m).to_decimal(), "9");
        assert_eq!(a.neg().py_mod(&m).to_decimal(), "34359738362");
    }

    #[test]
    fn conversions_round_as_python() {
        assert_eq!(int("9007199254740993").to_f64(), Ok(9007199254740992.0));
        assert_eq!(int("9007199254740995").to_f64(), Ok(9007199254740996.0));
        assert_eq!(int("10").pow(23).unwrap().to_f64(), Ok(1e23));
        let huge = int("10").pow(100).unwrap().pow(4).unwrap();
        assert_eq!(huge.to_f64(), Err(TOO_LARGE.into()));
        assert_eq!(int("7").true_div(&int("2")), Ok(3.5));
        assert_eq!(int("1").true_div(&int("3")), Ok(1.0 / 3.0));
        let t = int("10").pow(30).unwrap();
        assert_eq!(t.true_div(&int("3")), Ok(3.333333333333333e29));
        assert_eq!(
            int("0").true_div(&int("-5")).map(f64::is_sign_negative),
            Ok(true)
        );
        assert_eq!(
            BigInt::from_f64_integral(1e20).to_decimal(),
            "100000000000000000000"
        );
        assert_eq!(BigInt::from_f64_integral(-3.0).to_decimal(), "-3");
    }

    #[test]
    fn subnormal_quotients_round_once() {
        let two = int("2");
        let p1074 = two.pow(100).unwrap();
        let mut d = BigInt::from_u64(1);
        for _ in 0..10 {
            d = d.mul(&p1074).unwrap();
        }
        let d = d.mul(&two.pow(74).unwrap()).unwrap(); // 2**1074
        assert_eq!(int("1").true_div(&d), Ok(f64::from_bits(1)));
        let d2 = d.mul(&two).unwrap(); // 2**1075: exactly half, ties to even
        assert_eq!(int("1").true_div(&d2), Ok(0.0));
        assert_eq!(int("3").true_div(&d2), Ok(f64::from_bits(2)));
    }
}
