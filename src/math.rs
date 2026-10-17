//! Pure integer accounting shared by the program and host tests. No floats, no panics.

pub const SCALE: u128 = 1_000_000_000_000;
pub const BPS: u64 = 10_000;

/// Running totals of one term. Every share is derived from the cumulative income `f`,
/// so splitting income into many small collects never rounds the platform or buyers down.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Book {
    pub f: u64,         // income booked inside the term
    pub plat: u64,      // floor(f * plat_bps / BPS)
    pub index: u128,    // floor(pool(f) * SCALE / units_total), pool = (f - plat) * share / BPS
    pub acc: u128,      // sum over collects of units_sold * index increment (SCALE units)
    pub reserved: u64,  // floor(acc / SCALE): upper bound of everything holders can ever claim
}

#[derive(Debug, PartialEq, Eq)]
pub struct Step {
    pub plat: u64,
    pub buyers: u64,
    pub seller: u64,
}

pub fn mul_div(a: u64, b: u64, c: u64) -> Option<u64> {
    if c == 0 {
        return None;
    }
    u64::try_from((a as u128).checked_mul(b as u128)? / c as u128).ok()
}

/// Lamports earned by `units` between two index values (floored).
pub fn earned(units: u32, index: u128, entry: u128) -> Option<u64> {
    u64::try_from(index.checked_sub(entry)?.checked_mul(units as u128)? / SCALE).ok()
}

impl Book {
    /// Book `amount` collected inside the term. Holders' claims sum to at most
    /// floor(acc / SCALE) = `reserved`, so buyers' debt never exceeds what is set aside.
    /// The seller share can be short by a lamport or two on dust-sized amounts;
    /// callers book only amounts >= MIN_COLLECT where that cannot happen (returns None otherwise).
    pub fn add(&mut self, amount: u64, plat_bps: u16, share_bps: u16, units: u32, sold: u32) -> Option<Step> {
        if units == 0 || sold > units || plat_bps as u64 > BPS || share_bps as u64 > BPS {
            return None;
        }
        let f = self.f.checked_add(amount)?;
        let plat = mul_div(f, plat_bps as u64, BPS)?;
        let pool = mul_div(f.checked_sub(plat)?, share_bps as u64, BPS)?;
        let index = (pool as u128).checked_mul(SCALE)? / units as u128;
        let delta = index.checked_sub(self.index)?;
        let acc = self.acc.checked_add(delta.checked_mul(sold as u128)?)?;
        let reserved = u64::try_from(acc / SCALE).ok()?;
        let step_plat = plat.checked_sub(self.plat)?;
        let buyers = reserved.checked_sub(self.reserved)?;
        let seller = amount.checked_sub(step_plat)?.checked_sub(buyers)?;
        *self = Book { f, plat, index, acc, reserved };
        Some(Step { plat: step_plat, buyers, seller })
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec::Vec;

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            self.0 >> 33
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    #[test]
    fn example_from_money_doc() {
        // 10 SOL, 3000 bps sold out: platform 0.2, buyers 2.94, seller 6.86.
        let mut b = Book::default();
        let s = b.add(10_000_000_000, 200, 3000, 1000, 1000).unwrap();
        assert_eq!(s, Step { plat: 200_000_000, buyers: 2_940_000_000, seller: 6_860_000_000 });
    }

    #[test]
    fn unsold_share_goes_to_seller() {
        let mut b = Book::default();
        let s = b.add(1_000_000_000, 200, 5000, 100, 25).unwrap();
        assert_eq!(s.plat, 20_000_000);
        assert_eq!(s.buyers, 490_000_000 / 4);
        assert_eq!(s.plat + s.buyers + s.seller, 1_000_000_000);
    }

    #[test]
    fn many_small_collects_keep_platform_share() {
        let mut b = Book::default();
        let mut plat = 0;
        for _ in 0..1000 {
            plat += b.add(1_049, 200, 3000, 1000, 1000).unwrap().plat;
        }
        assert_eq!(plat, 1_049_000 * 200 / 10_000);
    }

    #[test]
    fn rejects_bad_inputs() {
        let mut b = Book::default();
        assert!(b.add(1, 200, 3000, 0, 0).is_none());
        assert!(b.add(1, 200, 3000, 10, 11).is_none());
        assert!(b.add(1, 10_001, 3000, 10, 1).is_none());
        b.f = u64::MAX;
        assert!(b.add(1, 200, 3000, 10, 1).is_none());
    }

    /// Random collects (>= MIN), random buys, random claims: conservation, monotone shares,
    /// claims never exceed the reserve, platform total exact.
    #[test]
    fn solvency_simulation() {
        for seed in 1..400u64 {
            let mut r = Rng(seed);
            let units = 10 + r.below(10_000) as u32;
            let share = 500 + r.below(4501) as u16;
            let mut b = Book::default();
            let mut sold = 0u32;
            // (units, entry, accrued, claimed)
            let mut pos: Vec<(u32, u128, u64, u64)> = Vec::new();
            let (mut total_plat, mut total_buyers, mut total_seller, mut claimed) = (0u64, 0u64, 0u64, 0u64);
            for _ in 0..60 {
                match r.below(3) {
                    0 => {
                        let amount = if r.below(4) == 0 { 1_000 + r.below(50) } else { r.below(50_000_000_000) + 1_000 };
                        let s = b.add(amount, 200, share, units, sold).unwrap();
                        assert_eq!(s.plat + s.buyers + s.seller, amount);
                        total_plat += s.plat;
                        total_buyers += s.buyers;
                        total_seller += s.seller;
                    }
                    1 if sold < units => {
                        let n = 1 + r.below((units - sold) as u64) as u32;
                        if !pos.is_empty() && r.below(2) == 0 {
                            let i = r.below(pos.len() as u64) as usize;
                            let p = &mut pos[i];
                            p.2 += earned(p.0, b.index, p.1).unwrap();
                            p.1 = b.index;
                            p.0 += n;
                        } else {
                            pos.push((n, b.index, 0, 0));
                        }
                        sold += n;
                    }
                    _ if !pos.is_empty() => {
                        let i = r.below(pos.len() as u64) as usize;
                        let p = &mut pos[i];
                        let due = p.2 + earned(p.0, b.index, p.1).unwrap();
                        p.2 = 0;
                        p.1 = b.index;
                        p.3 += due;
                        claimed += due;
                        assert!(claimed <= b.reserved, "claims exceed reserve");
                    }
                    _ => {}
                }
            }
            let outstanding: u64 = pos.iter().map(|p| p.2 + earned(p.0, b.index, p.1).unwrap()).sum();
            assert!(claimed + outstanding <= b.reserved, "seed {seed}: debt exceeds reserve");
            assert_eq!(total_buyers, b.reserved);
            assert_eq!(total_plat, b.f as u128 as u64 * 200 / 10_000);
            assert_eq!(total_plat + total_buyers + total_seller, b.f);
            // Dust left for the creator after every holder is paid stays tiny.
            assert!(b.reserved - claimed - outstanding <= pos.len() as u64 + 60);
        }
    }

    #[test]
    fn min_collect_never_shorts_seller() {
        // Worst case: tiny amounts at maximum share, odd unit counts.
        for units in [10u32, 7_777, 9_999, 10_000] {
            let mut b = Book::default();
            for i in 0..5_000u64 {
                assert!(b.add(1_000 + i % 7, 200, 5000, units, units).is_some());
            }
        }
    }
}
