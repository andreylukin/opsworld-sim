//! SplitMix64. Implemented here so the stream can never change with a
//! dependency upgrade, which would silently invalidate saved repro files.

#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    /// Derives an independent stream, so one consumer cannot shift another.
    pub fn fork(seed: u64, stream: u64) -> Self {
        let mut mixer = Self(seed ^ stream.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        Self(mixer.next_u64())
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    pub fn below(&mut self, n: u32) -> u32 {
        (self.next_u64() % u64::from(n)) as u32
    }

    /// Uniform in `lo..=hi`.
    pub fn range(&mut self, lo: u32, hi: u32) -> u32 {
        lo + self.below(hi - lo + 1)
    }

    pub fn chance(&mut self, percent: u32) -> bool {
        self.below(100) < percent
    }
}
