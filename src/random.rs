//! MRI's pseudo-random number generator, ported from `random.c` and
//! `missing/mt19937.c` (ruby 4.0).
//!
//! `srand(42); rand(100)` is part of a program's observable output: seeded
//! tests, shuffled fixtures and sampled data all print it. The generator, the
//! seeding of its state from an Integer, the bounded-integer rejection loop and
//! the 53-bit float construction are each transcribed from MRI so a seeded run
//! answers exactly what `ruby` answers.
//!
//! Two kinds of stream exist, as in MRI: the process-wide default (behind
//! `Kernel#rand`, `Random.rand`, `Array#shuffle`/`#sample`) and one per
//! `Random.new` instance, keyed here by the instance's heap id.

use num_bigint::{BigInt, Sign};
use std::cell::RefCell;
use std::collections::HashMap;

const N: usize = 624;
const M: usize = 397;
const MATRIX_A: u32 = 0x9908_b0df;
const UMASK: u32 = 0x8000_0000;
const LMASK: u32 = 0x7fff_ffff;

fn twist(u: u32, v: u32) -> u32 {
    (((u & UMASK) | (v & LMASK)) >> 1) ^ if v & 1 != 0 { MATRIX_A } else { 0 }
}

/// The MT19937 state (`struct MT`). `next` indexes `state`; `left` counts the
/// words remaining before the next twist, exactly as MRI's pointer pair does.
#[derive(Clone)]
pub struct Mt {
    state: [u32; N],
    next: usize,
    left: i32,
}

impl Mt {
    /// `init_genrand`.
    fn init_genrand(s: u32) -> Mt {
        let mut state = [0u32; N];
        state[0] = s;
        for j in 1..N {
            state[j] = 1_812_433_253u32
                .wrapping_mul(state[j - 1] ^ (state[j - 1] >> 30))
                .wrapping_add(j as u32);
        }
        Mt {
            state,
            next: N,
            left: 1,
        }
    }

    /// `init_by_array`.
    fn init_by_array(key: &[u32]) -> Mt {
        let mut mt = Mt::init_genrand(19_650_218);
        let st = &mut mt.state;
        let (mut i, mut j) = (1usize, 0usize);
        let mut k = N.max(key.len());
        while k > 0 {
            st[i] = (st[i] ^ (st[i - 1] ^ (st[i - 1] >> 30)).wrapping_mul(1_664_525))
                .wrapping_add(key[j])
                .wrapping_add(j as u32);
            i += 1;
            j += 1;
            if i >= N {
                st[0] = st[N - 1];
                i = 1;
            }
            if j >= key.len() {
                j = 0;
            }
            k -= 1;
        }
        k = N - 1;
        while k > 0 {
            st[i] = (st[i] ^ (st[i - 1] ^ (st[i - 1] >> 30)).wrapping_mul(1_566_083_941))
                .wrapping_sub(i as u32);
            i += 1;
            if i >= N {
                st[0] = st[N - 1];
                i = 1;
            }
            k -= 1;
        }
        st[0] = 0x8000_0000;
        mt
    }

    /// `next_state`: regenerate the whole block of N words.
    fn next_state(&mut self) {
        let s = &mut self.state;
        self.left = N as i32;
        self.next = 0;
        for p in 0..N - M {
            s[p] = s[p + M] ^ twist(s[p], s[p + 1]);
        }
        for p in N - M..N - 1 {
            s[p] = s[p + M - N] ^ twist(s[p], s[p + 1]);
        }
        s[N - 1] = s[M - 1] ^ twist(s[N - 1], s[0]);
    }

    /// `genrand_int32`.
    fn genrand_int32(&mut self) -> u32 {
        self.left -= 1;
        if self.left <= 0 {
            self.next_state();
        }
        let mut y = self.state[self.next];
        self.next += 1;
        y ^= y >> 11;
        y ^= (y << 7) & 0x9d2c_5680;
        y ^= (y << 15) & 0xefc6_0000;
        y ^= y >> 18;
        y
    }

    /// `rand_init`: seed from an Integer. Its magnitude is split into 32-bit
    /// words, least significant first; a single word seeds through
    /// `init_genrand`, several through `init_by_array` (after dropping a top
    /// word of exactly 1, MRI's "leading-zero-guard").
    pub fn from_seed(seed: &BigInt) -> Mt {
        let (_, mut words) = seed.to_u32_digits();
        if words.is_empty() {
            words.push(0);
        }
        if words.len() == 1 {
            return Mt::init_genrand(words[0]);
        }
        if words.last() == Some(&1) {
            words.pop();
        }
        Mt::init_by_array(&words)
    }
}

/// Which stream a draw comes from.
#[derive(Clone, Copy)]
pub enum Gen {
    /// The process-wide generator behind `Kernel#rand`.
    Default,
    /// A `Random.new` instance, by heap id.
    Instance(u32),
}

thread_local! {
    /// The default generator and the seed it was last given (`srand`'s answer).
    static DEFAULT: RefCell<Option<(Mt, BigInt)>> = const { RefCell::new(None) };
    /// Per-instance generators.
    static INSTANCES: RefCell<HashMap<u32, Mt>> = RefCell::new(HashMap::new());
}

/// A fresh 128-bit seed, as `Random.new_seed` / an unseeded generator use. MRI
/// reads it from the OS entropy source; so does this (through the hasher keys
/// `RandomState` draws from it), mixed with the clock.
pub fn new_seed() -> BigInt {
    use std::hash::{BuildHasher, Hasher};
    let mut words = [0u32; 4];
    for (i, w) in words.iter_mut().enumerate() {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        );
        h.write_usize(i);
        *w = h.finish() as u32;
    }
    BigInt::from_slice(Sign::Plus, &words)
}

fn with_mt<R>(g: Gen, f: impl FnOnce(&mut Mt) -> R) -> R {
    match g {
        Gen::Default => DEFAULT.with(|d| {
            let mut d = d.borrow_mut();
            let (mt, _) = d.get_or_insert_with(|| {
                let seed = new_seed();
                (Mt::from_seed(&seed), seed)
            });
            f(mt)
        }),
        Gen::Instance(id) => INSTANCES.with(|m| {
            let mut m = m.borrow_mut();
            let mt = m.entry(id).or_insert_with(|| Mt::from_seed(&new_seed()));
            f(mt)
        }),
    }
}

/// `srand(seed)`: reseed the default generator, answering the previous seed.
pub fn srand(seed: BigInt) -> BigInt {
    let mt = Mt::from_seed(&seed);
    DEFAULT.with(|d| {
        let old = d.borrow_mut().replace((mt, seed));
        match old {
            Some((_, s)) => s,
            None => new_seed(),
        }
    })
}

/// Seed the generator of a `Random.new(seed)` instance.
pub fn seed_instance(id: u32, seed: &BigInt) {
    let mt = Mt::from_seed(seed);
    INSTANCES.with(|m| m.borrow_mut().insert(id, mt));
}

/// One 32-bit draw.
pub fn int32(g: Gen) -> u32 {
    with_mt(g, |mt| mt.genrand_int32())
}

fn make_mask(mut x: u64) -> u64 {
    x |= x >> 1;
    x |= x >> 2;
    x |= x >> 4;
    x |= x >> 8;
    x |= x >> 16;
    x |= x >> 32;
    x
}

/// `limited_rand`: uniform in `0..=limit`, by masking 32-bit draws and
/// rejecting anything above the limit.
pub fn limited(g: Gen, limit: u64) -> u64 {
    if limit == 0 {
        return 0;
    }
    let mask = make_mask(limit);
    with_mt(g, |mt| {
        if limit > 0xffff_ffff {
            'retry: loop {
                let mut val = 0u64;
                for i in (0..=1).rev() {
                    if (mask >> (i * 32)) & 0xffff_ffff != 0 {
                        val |= (mt.genrand_int32() as u64) << (i * 32);
                        val &= mask;
                        if limit < val {
                            continue 'retry;
                        }
                    }
                }
                return val;
            }
        }
        loop {
            let val = mt.genrand_int32() as u64 & mask;
            if val <= limit {
                return val;
            }
        }
    })
}

/// `limited_big_rand`: uniform in `0..=limit` for a limit wider than 64 bits,
/// drawing one 32-bit word per word of the limit, most significant first.
pub fn limited_big(g: Gen, limit: &BigInt) -> BigInt {
    let (_, lim) = limit.to_u32_digits();
    let len = lim.len();
    let mut out = vec![0u32; len];
    with_mt(g, |mt| 'retry: loop {
        let mut mask = 0u32;
        let mut boundary = true;
        for i in (0..len).rev() {
            let l = lim[i];
            mask = if mask != 0 {
                0xffff_ffff
            } else {
                make_mask(l as u64) as u32
            };
            let mut r = 0u32;
            if mask != 0 {
                r = mt.genrand_int32() & mask;
                if boundary {
                    if l < r {
                        continue 'retry;
                    }
                    if r < l {
                        boundary = false;
                    }
                }
            }
            out[i] = r;
        }
        return;
    });
    BigInt::from_slice(Sign::Plus, &out)
}

/// `random_real`: a Float from two 32-bit draws — `[0, 1)` when `excl`, else
/// `[0, 1]` (`int_pair_to_real_exclusive` / `_inclusive`).
pub fn real(g: Gen, excl: bool) -> f64 {
    let (a, b) = with_mt(g, |mt| (mt.genrand_int32(), mt.genrand_int32()));
    if excl {
        let (a, b) = (a >> 5, b >> 6);
        (a as f64 * (1u64 << 26) as f64 + b as f64) * (1.0 / (1u64 << 53) as f64)
    } else {
        let m: u128 = (1u128 << 53) | 1;
        let x: u128 = ((a as u128) << 32) | b as u128;
        let r = ((x.wrapping_mul(m)) >> 64) as u64 as f64;
        r * (1.0 / (1u64 << 53) as f64)
    }
}
