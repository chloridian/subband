const STATE_MASK: u8 = 0x3F;
const QE_TABLE_LEN: usize = STATE_MASK as usize + 1;

static QE_TABLE: [QeData; QE_TABLE_LEN] = padded(QE_STATES);

#[derive(Copy, Clone)]
pub(crate) struct ArithmeticDecoder<'a> {
    data: &'a [u8],
    c: u32,
    a: u32,
    shift_count: u32,
}

impl<'a> ArithmeticDecoder<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        let mut decoder = ArithmeticDecoder {
            data,
            c: 0,
            a: 0,
            shift_count: 0,
        };

        decoder.initialize();

        decoder
    }

    #[inline(always)]
    pub(crate) fn read_bit(&mut self, context: &mut ArithmeticDecoderContext) -> u32 {
        self.decode(context)
    }

    fn initialize(&mut self) {
        let b0 = self.data.first().copied().unwrap_or(0xff);
        self.c = ((b0 as u32) ^ 0xff) << 16;
        self.read_byte();

        self.c <<= 7;
        self.shift_count -= 7;
        self.a = 0x8000;
    }

    #[inline(always)]
    fn read_byte(&mut self) {
        if let [b0, ref rest @ ..] = *self.data
            && let [b1, ..] = *rest
        {
            if b0 == 0xff {
                if b1 > 0x8f {
                    self.shift_count = 8;
                } else {
                    self.data = rest;
                    self.c = self.c.wrapping_add(0xfe00).wrapping_sub((b1 as u32) << 9);
                    self.shift_count = 7;
                }
            } else {
                self.data = rest;
                self.c = self.c.wrapping_add(0xff00).wrapping_sub((b1 as u32) << 8);
                self.shift_count = 8;
            }
        } else {
            self.shift_count = 8;
        }
    }

    #[inline(always)]
    fn renormalize(&mut self) {
        while self.a & 0x8000 == 0 {
            if self.shift_count == 0 {
                self.read_byte();
            }

            let shifts_needed = self.a.leading_zeros() - 16;
            let batch = shifts_needed.min(self.shift_count);
            self.a <<= batch;
            self.c <<= batch;
            self.shift_count -= batch;
        }
    }

    #[inline(always)]
    #[expect(clippy::indexing_slicing)]
    pub(crate) fn decode(&mut self, context: &mut ArithmeticDecoderContext) -> u32 {
        let qe_entry = QE_TABLE[context.index() as usize];
        let qe = u32::from(qe_entry.qe);

        self.a -= qe;
        let a_shifted = self.a << 16;

        if self.a >= 0x8000 && self.c < a_shifted {
            return context.mps();
        }

        let is_lps = (self.c >= a_shifted) as u32;
        let lps_mask = is_lps.wrapping_neg();
        self.c -= a_shifted & lps_mask;

        let cond = (self.a < qe) as u32;
        self.a = (self.a & !lps_mask) | (qe & lps_mask);

        let flip = cond ^ is_lps;
        let d = context.mps() ^ flip;

        let pick_nlps = (flip as u8).wrapping_neg();
        let new_mps = (context.0 ^ (qe_entry.nlps_and_switch & pick_nlps)) & 0x80;
        let new_idx =
            qe_entry.nmps ^ ((qe_entry.nmps ^ (qe_entry.nlps_and_switch & STATE_MASK)) & pick_nlps);
        context.0 = new_mps | new_idx;

        self.renormalize();

        d
    }
}

#[derive(Copy, Clone, Debug, Default)]
pub(crate) struct ArithmeticDecoderContext(u8);

impl ArithmeticDecoderContext {
    #[inline(always)]
    pub(crate) fn index(self) -> u32 {
        (self.0 & STATE_MASK) as u32
    }

    #[inline(always)]
    pub(crate) fn mps(self) -> u32 {
        (self.0 >> 7) as u32
    }

    #[inline(always)]
    pub(crate) fn reset(&mut self) {
        self.0 = 0;
    }

    #[inline(always)]
    pub(crate) fn reset_with_index(&mut self, index: u8) {
        self.0 = index;
    }
}

#[derive(Debug, Clone, Copy)]
struct QeData {
    qe: u16,
    nmps: u8,
    nlps_and_switch: u8,
}

macro_rules! qe {
    ($($qe:expr, $nmps:expr, $nlps:expr, $switch:expr),+ $(,)?) => {
        [
            $(
                QeData {
                    qe: $qe,
                    nmps: $nmps,
                    nlps_and_switch: $nlps | (($switch as u8) << 7),
                }
            ),+
        ]
    };
}

#[rustfmt::skip]
const QE_STATES: [QeData; 47] = qe!(
    0x5601, 1, 1, true,
    0x3401, 2, 6, false,
    0x1801, 3, 9, false,
    0x0AC1, 4, 12, false,
    0x0521, 5, 29, false,
    0x0221, 38, 33, false,
    0x5601, 7, 6, true,
    0x5401, 8, 14, false,
    0x4801, 9, 14, false,
    0x3801, 10, 14, false,
    0x3001, 11, 17, false,
    0x2401, 12, 18, false,
    0x1C01, 13, 20, false,
    0x1601, 29, 21, false,
    0x5601, 15, 14, true,
    0x5401, 16, 14, false,
    0x5101, 17, 15, false,
    0x4801, 18, 16, false,
    0x3801, 19, 17, false,
    0x3401, 20, 18, false,
    0x3001, 21, 19, false,
    0x2801, 22, 19, false,
    0x2401, 23, 20, false,
    0x2201, 24, 21, false,
    0x1C01, 25, 22, false,
    0x1801, 26, 23, false,
    0x1601, 27, 24, false,
    0x1401, 28, 25, false,
    0x1201, 29, 26, false,
    0x1101, 30, 27, false,
    0x0AC1, 31, 28, false,
    0x09C1, 32, 29, false,
    0x08A1, 33, 30, false,
    0x0521, 34, 31, false,
    0x0441, 35, 32, false,
    0x02A1, 36, 33, false,
    0x0221, 37, 34, false,
    0x0141, 38, 35, false,
    0x0111, 39, 36, false,
    0x0085, 40, 37, false,
    0x0049, 41, 38, false,
    0x0025, 42, 39, false,
    0x0015, 43, 40, false,
    0x0009, 44, 41, false,
    0x0005, 45, 42, false,
    0x0001, 45, 43, false,
    0x5601, 46, 46, false,
);

#[expect(clippy::indexing_slicing)]
const fn padded(states: [QeData; 47]) -> [QeData; QE_TABLE_LEN] {
    let mut table = [states[46]; QE_TABLE_LEN];
    let mut i = 0;
    while i < states.len() {
        table[i] = states[i];
        i += 1;
    }
    table
}
