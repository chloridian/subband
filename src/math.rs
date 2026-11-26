use core::ops::{Deref, DerefMut};
use std::vec::Vec;

use crate::cancel::Cancel;
use crate::error::Result;

pub(crate) const SIMD_WIDTH: usize = 8;

#[cfg(feature = "simd")]
mod inner {
    use super::SIMD_WIDTH;
    use core::ops::{Add, AddAssign, DivAssign, Mul, MulAssign, Sub, SubAssign};
    use fearless_simd::{SimdBase, SimdFloat};

    pub(crate) use fearless_simd::{Level, Simd, dispatch};

    #[derive(Copy, Clone)]
    #[repr(C, align(32))]
    pub(crate) struct f32x8<S: Simd> {
        inner: fearless_simd::f32x8<S>,
    }

    impl<S: Simd> f32x8<S> {
        #[inline(always)]
        pub(crate) fn from_slice(simd: S, slice: &[f32]) -> Self {
            Self {
                inner: fearless_simd::f32x8::from_slice(simd, slice),
            }
        }

        #[inline(always)]
        pub(crate) fn splat(simd: S, value: f32) -> Self {
            Self {
                inner: fearless_simd::f32x8::splat(simd, value),
            }
        }

        #[inline(always)]
        pub(crate) fn mul_add(self, mul: Self, addend: Self) -> Self {
            Self {
                inner: self.inner.mul_add(mul.inner, addend.inner),
            }
        }

        #[inline(always)]
        pub(crate) fn floor(self) -> Self {
            Self {
                inner: self.inner.floor(),
            }
        }

        #[inline(always)]
        pub(crate) fn store(self, slice: &mut [f32]) {
            if let Some(dst) = slice.get_mut(..SIMD_WIDTH) {
                self.inner.store_slice(dst);
            }
        }

        #[inline(always)]
        pub(crate) fn zip_low(self, other: Self) -> Self {
            Self {
                inner: self.inner.zip_low(other.inner),
            }
        }

        #[inline(always)]
        pub(crate) fn zip_high(self, other: Self) -> Self {
            Self {
                inner: self.inner.zip_high(other.inner),
            }
        }
    }

    impl<S: Simd> Add for f32x8<S> {
        type Output = Self;
        #[inline(always)]
        fn add(self, rhs: Self) -> Self {
            Self {
                inner: self.inner + rhs.inner,
            }
        }
    }

    impl<S: Simd> Sub for f32x8<S> {
        type Output = Self;
        #[inline(always)]
        fn sub(self, rhs: Self) -> Self {
            Self {
                inner: self.inner - rhs.inner,
            }
        }
    }

    impl<S: Simd> Mul for f32x8<S> {
        type Output = Self;
        #[inline(always)]
        fn mul(self, rhs: Self) -> Self {
            Self {
                inner: self.inner * rhs.inner,
            }
        }
    }

    impl<S: Simd> Add<f32> for f32x8<S> {
        type Output = Self;
        #[inline(always)]
        fn add(self, rhs: f32) -> Self {
            Self {
                inner: self.inner + rhs,
            }
        }
    }

    impl<S: Simd> Mul<f32> for f32x8<S> {
        type Output = Self;
        #[inline(always)]
        fn mul(self, rhs: f32) -> Self {
            Self {
                inner: self.inner * rhs,
            }
        }
    }

    impl<S: Simd> AddAssign for f32x8<S> {
        #[inline(always)]
        fn add_assign(&mut self, rhs: Self) {
            self.inner = self.inner + rhs.inner;
        }
    }

    impl<S: Simd> SubAssign for f32x8<S> {
        #[inline(always)]
        fn sub_assign(&mut self, rhs: Self) {
            self.inner = self.inner - rhs.inner;
        }
    }

    impl<S: Simd> MulAssign<f32> for f32x8<S> {
        #[inline(always)]
        fn mul_assign(&mut self, rhs: f32) {
            self.inner = self.inner * rhs;
        }
    }

    impl<S: Simd> DivAssign<f32> for f32x8<S> {
        #[inline(always)]
        fn div_assign(&mut self, rhs: f32) {
            self.inner = self.inner / rhs;
        }
    }

    #[inline(always)]
    pub(crate) fn round_and_interleave<S: Simd, const N: usize>(
        simd: S,
        channels: [&[f32; 16]; N],
        out: &mut [u8],
    ) {
        match N {
            1 => to_u8(simd, channels[0]).store_slice(out),
            2 => {
                let (low, high) =
                    simd.interleave_u8x16(to_u8(simd, channels[0]), to_u8(simd, channels[1]));
                let (out_low, out_high) = out.split_at_mut(16);
                low.store_slice(out_low);
                high.store_slice(out_high);
            }
            3 => {
                let (c0, c1, c2) = (
                    to_u8(simd, channels[0]),
                    to_u8(simd, channels[1]),
                    to_u8(simd, channels[2]),
                );
                for (out, [i0, i1, i2]) in out.as_chunks_mut::<16>().0.iter_mut().zip(THREE_WAY) {
                    let interleaved = c0.swizzle_dyn_precise(i0)
                        | c1.swizzle_dyn_precise(i1)
                        | c2.swizzle_dyn_precise(i2);
                    interleaved.store_slice(out);
                }
            }
            4 => {
                let channels = [
                    to_u8(simd, channels[0]),
                    to_u8(simd, channels[1]),
                    to_u8(simd, channels[2]),
                    to_u8(simd, channels[3]),
                ];
                if let Ok(out) = out.try_into() {
                    simd.store_four_interleaved_u8x16(channels, out);
                }
            }
            _ => unreachable!(),
        }
    }

    #[inline(always)]
    fn to_u8<S: Simd>(simd: S, values: &[f32; 16]) -> fearless_simd::u8x16<S> {
        let half = fearless_simd::f32x8::splat(simd, 0.5);
        let (lo, hi) = values.split_at(8);
        let convert = |chunk: &[f32]| {
            let v = fearless_simd::f32x8::from_slice(simd, chunk) + half;

            #[cfg(target_arch = "aarch64")]
            {
                let (a, b) = simd.split_u32x8(simd.cvt_u32_precise_f32x8(v));
                simd.saturating_narrow_u32x4(a, b)
            }

            #[cfg(not(target_arch = "aarch64"))]
            {
                use fearless_simd::Bytes;

                let zero = fearless_simd::f32x8::splat(simd, 0.0);
                let max = fearless_simd::f32x8::splat(simd, 255.0);
                let (a, b) = simd
                    .split_i32x8(simd.cvt_i32_f32x8(simd.min_f32x8(simd.max_f32x8(v, zero), max)));
                simd.relaxed_narrow_i32x4(a, b).bitcast()
            }
        };
        simd.saturating_narrow_u16x8(convert(lo), convert(hi))
    }

    const THREE_WAY: [[[u8; 16]; 3]; 3] = [
        [
            [
                0, 0x80, 0x80, 1, 0x80, 0x80, 2, 0x80, 0x80, 3, 0x80, 0x80, 4, 0x80, 0x80, 5,
            ],
            [
                0x80, 0, 0x80, 0x80, 1, 0x80, 0x80, 2, 0x80, 0x80, 3, 0x80, 0x80, 4, 0x80, 0x80,
            ],
            [
                0x80, 0x80, 0, 0x80, 0x80, 1, 0x80, 0x80, 2, 0x80, 0x80, 3, 0x80, 0x80, 4, 0x80,
            ],
        ],
        [
            [
                0x80, 0x80, 6, 0x80, 0x80, 7, 0x80, 0x80, 8, 0x80, 0x80, 9, 0x80, 0x80, 10, 0x80,
            ],
            [
                5, 0x80, 0x80, 6, 0x80, 0x80, 7, 0x80, 0x80, 8, 0x80, 0x80, 9, 0x80, 0x80, 10,
            ],
            [
                0x80, 5, 0x80, 0x80, 6, 0x80, 0x80, 7, 0x80, 0x80, 8, 0x80, 0x80, 9, 0x80, 0x80,
            ],
        ],
        [
            [
                0x80, 11, 0x80, 0x80, 12, 0x80, 0x80, 13, 0x80, 0x80, 14, 0x80, 0x80, 15, 0x80,
                0x80,
            ],
            [
                0x80, 0x80, 11, 0x80, 0x80, 12, 0x80, 0x80, 13, 0x80, 0x80, 14, 0x80, 0x80, 15,
                0x80,
            ],
            [
                10, 0x80, 0x80, 11, 0x80, 0x80, 12, 0x80, 0x80, 13, 0x80, 0x80, 14, 0x80, 0x80, 15,
            ],
        ],
    ];
}

#[cfg(not(feature = "simd"))]
mod inner {
    use super::SIMD_WIDTH;
    use core::marker::PhantomData;
    use core::ops::{Add, AddAssign, DivAssign, Mul, MulAssign, Sub, SubAssign};

    pub(crate) trait Simd: Copy + Clone {}

    #[derive(Copy, Clone)]
    pub(crate) struct ScalarSimd;
    impl Simd for ScalarSimd {}

    pub(crate) struct Level;
    impl Level {
        #[inline(always)]
        pub(crate) fn new() -> Self {
            Self
        }
    }

    #[derive(Copy, Clone)]
    #[repr(C, align(32))]
    pub(crate) struct f32x8<S: Simd> {
        val: [f32; SIMD_WIDTH],
        _marker: PhantomData<S>,
    }

    impl<S: Simd> f32x8<S> {
        #[inline(always)]
        pub(crate) fn from_slice(_simd: S, slice: &[f32]) -> Self {
            let mut val = [0.0_f32; SIMD_WIDTH];
            if let Some(src) = slice.get(..SIMD_WIDTH) {
                val.copy_from_slice(src);
            }
            Self {
                val,
                _marker: PhantomData,
            }
        }

        #[inline(always)]
        pub(crate) fn splat(_simd: S, value: f32) -> Self {
            Self {
                val: [value; SIMD_WIDTH],
                _marker: PhantomData,
            }
        }

        #[inline(always)]
        pub(crate) fn mul_add(self, mul: Self, addend: Self) -> Self {
            let mut val = self.val;
            for ((a, b), c) in val.iter_mut().zip(mul.val).zip(addend.val) {
                *a = super::mul_add(*a, b, c);
            }
            Self {
                val,
                _marker: PhantomData,
            }
        }

        #[inline(always)]
        pub(crate) fn floor(self) -> Self {
            Self {
                val: self.val.map(f32::floor),
                _marker: PhantomData,
            }
        }

        #[inline(always)]
        pub(crate) fn store(self, slice: &mut [f32]) {
            if let Some(dst) = slice.get_mut(..SIMD_WIDTH) {
                dst.copy_from_slice(&self.val);
            }
        }

        #[inline(always)]
        pub(crate) fn zip_low(self, other: Self) -> Self {
            Self {
                val: [
                    self.val[0],
                    other.val[0],
                    self.val[1],
                    other.val[1],
                    self.val[2],
                    other.val[2],
                    self.val[3],
                    other.val[3],
                ],
                _marker: PhantomData,
            }
        }

        #[inline(always)]
        pub(crate) fn zip_high(self, other: Self) -> Self {
            Self {
                val: [
                    self.val[4],
                    other.val[4],
                    self.val[5],
                    other.val[5],
                    self.val[6],
                    other.val[6],
                    self.val[7],
                    other.val[7],
                ],
                _marker: PhantomData,
            }
        }
    }

    impl<S: Simd> Add for f32x8<S> {
        type Output = Self;
        #[inline(always)]
        fn add(self, rhs: Self) -> Self {
            let mut val = self.val;
            for (a, b) in val.iter_mut().zip(rhs.val) {
                *a += b;
            }
            Self {
                val,
                _marker: PhantomData,
            }
        }
    }

    impl<S: Simd> Sub for f32x8<S> {
        type Output = Self;
        #[inline(always)]
        fn sub(self, rhs: Self) -> Self {
            let mut val = self.val;
            for (a, b) in val.iter_mut().zip(rhs.val) {
                *a -= b;
            }
            Self {
                val,
                _marker: PhantomData,
            }
        }
    }

    impl<S: Simd> Mul for f32x8<S> {
        type Output = Self;
        #[inline(always)]
        fn mul(self, rhs: Self) -> Self {
            let mut val = self.val;
            for (a, b) in val.iter_mut().zip(rhs.val) {
                *a *= b;
            }
            Self {
                val,
                _marker: PhantomData,
            }
        }
    }

    impl<S: Simd> Add<f32> for f32x8<S> {
        type Output = Self;
        #[inline(always)]
        fn add(self, rhs: f32) -> Self {
            Self {
                val: self.val.map(|v| v + rhs),
                _marker: PhantomData,
            }
        }
    }

    impl<S: Simd> Mul<f32> for f32x8<S> {
        type Output = Self;
        #[inline(always)]
        fn mul(self, rhs: f32) -> Self {
            Self {
                val: self.val.map(|v| v * rhs),
                _marker: PhantomData,
            }
        }
    }

    impl<S: Simd> AddAssign for f32x8<S> {
        #[inline(always)]
        fn add_assign(&mut self, rhs: Self) {
            for (a, b) in self.val.iter_mut().zip(rhs.val) {
                *a += b;
            }
        }
    }

    impl<S: Simd> SubAssign for f32x8<S> {
        #[inline(always)]
        fn sub_assign(&mut self, rhs: Self) {
            for (a, b) in self.val.iter_mut().zip(rhs.val) {
                *a -= b;
            }
        }
    }

    impl<S: Simd> MulAssign<f32> for f32x8<S> {
        #[inline(always)]
        fn mul_assign(&mut self, rhs: f32) {
            for val in &mut self.val {
                *val *= rhs;
            }
        }
    }

    impl<S: Simd> DivAssign<f32> for f32x8<S> {
        #[inline(always)]
        fn div_assign(&mut self, rhs: f32) {
            for val in &mut self.val {
                *val /= rhs;
            }
        }
    }

    #[inline(always)]
    pub(crate) fn round_and_interleave<S: Simd, const N: usize>(
        _simd: S,
        channels: [&[f32; 16]; N],
        out: &mut [u8],
    ) {
        for (i, pixel) in out.as_chunks_mut::<N>().0.iter_mut().enumerate() {
            for (value, channel) in pixel.iter_mut().zip(channels) {
                if let Some(&sample) = channel.get(i) {
                    *value = super::round_to_u8(sample);
                }
            }
        }
    }

    #[macro_export]
    macro_rules! simd_dispatch {
        ($level:expr, $simd:ident => $body:expr) => {{
            let _ = $level;
            let $simd = $crate::math::ScalarSimd;
            $body
        }};
    }

    pub(crate) use simd_dispatch as dispatch;
}

#[inline(always)]
pub(crate) fn mul_add(a: f32, b: f32, c: f32) -> f32 {
    #[cfg(any(
        all(
            any(target_arch = "x86", target_arch = "x86_64"),
            target_feature = "fma"
        ),
        all(target_arch = "aarch64", target_feature = "neon")
    ))]
    {
        f32::mul_add(a, b, c)
    }
    #[cfg(not(any(
        all(
            any(target_arch = "x86", target_arch = "x86_64"),
            target_feature = "fma"
        ),
        all(target_arch = "aarch64", target_feature = "neon")
    )))]
    {
        a * b + c
    }
}

#[inline(always)]
pub(crate) fn round_to_u8(x: f32) -> u8 {
    (x + 0.5) as u8
}

#[inline(always)]
pub(crate) fn pow2i(exp: i32) -> f32 {
    if exp >= 0 {
        (1_u32 << exp) as f32
    } else {
        1.0 / (1_u32 << -exp) as f32
    }
}

pub(crate) use inner::*;

#[derive(Debug, Clone, Default)]
pub(crate) struct SimdBuffer<const N: usize> {
    data: Vec<f32>,
    original_len: usize,
}

impl<const N: usize> SimdBuffer<N> {
    pub(crate) fn resize_zero(&mut self, original_len: usize, cancel: Cancel<'_>) -> Result<()> {
        let padded_len = Self::padded_len(original_len);
        self.data.clear();
        cancel.resize(&mut self.data, padded_len)?;
        self.original_len = original_len;
        Ok(())
    }

    pub(crate) fn truncated(&self) -> &[f32] {
        self.data.get(..self.original_len).unwrap_or(&self.data)
    }

    fn padded_len(original_len: usize) -> usize {
        let remainder = original_len % N;
        let padding = N - remainder;
        original_len + padding
    }
}

impl<const N: usize> Deref for SimdBuffer<N> {
    type Target = [f32];

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.data
    }
}

impl<const N: usize> DerefMut for SimdBuffer<N> {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.data
    }
}
