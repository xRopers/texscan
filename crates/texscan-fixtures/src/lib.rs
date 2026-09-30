//! Deterministic fixtures for texscan tests: binary blobs with textures at known offsets,
//! plus traps that must not be reported. Everything comes from fixed seeds, so every run
//! produces the same bytes.
//!
//! `cargo run -p texscan-fixtures --bin gen-fixtures` writes them to `tests/fixtures/`.
//!
//! DDS headers are written here and sizes worked out here, independently of
//! texscan-core, so the tests compare two implementations.

/// xorshift64*: small, fast, and stable across platforms and releases.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub fn bytes(&mut self, n: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(n + 8);
        while out.len() < n {
            out.extend_from_slice(&self.next_u64().to_le_bytes());
        }
        out.truncate(n);
        out
    }
}

/// How a DDS header describes its pixel format.
#[derive(Debug, Clone, Copy)]
pub enum Pf {
    /// Legacy FourCC, such as `DXT1`.
    FourCC([u8; 4]),
    /// Legacy uncompressed: pixel format flags, bit count and R, G, B, A masks.
    Masks { flags: u32, bits: u32, masks: [u32; 4] },
    /// DX10 header with this DXGI format.
    Dx10(u32),
}

/// How much space pixels take, for the independent size calculation.
#[derive(Debug, Clone, Copy)]
pub enum Unit {
    /// 4×4 blocks of this many bytes.
    Block(u64),
    /// Bits per pixel.
    Bits(u64),
}

#[derive(Debug, Clone)]
pub struct DdsSpec {
    pub width: u32,
    pub height: u32,
    /// 1 unless a volume texture.
    pub depth: u32,
    /// As written in the header: 0 means 1.
    pub mips: u32,
    pub array_size: u32,
    pub cube: bool,
    pub pf: Pf,
    pub unit: Unit,
    /// The name texscan should report.
    pub name: &'static str,
    pub dxgi: Option<u32>,
}

impl DdsSpec {
    pub fn new(width: u32, height: u32, mips: u32, pf: Pf, unit: Unit, name: &'static str, dxgi: Option<u32>) -> Self {
        Self { width, height, depth: 1, mips, array_size: 1, cube: false, pf, unit, name, dxgi }
    }

    pub fn header_size(&self) -> usize {
        if matches!(self.pf, Pf::Dx10(_)) { 148 } else { 128 }
    }

    pub fn faces(&self) -> u32 {
        if self.cube { 6 } else { 1 }
    }

    pub fn data_size(&self) -> usize {
        let mut per_face = 0u64;
        for i in 0..self.mips.max(1) {
            let w = u64::from((self.width >> i).max(1));
            let h = u64::from((self.height >> i).max(1));
            let d = u64::from((self.depth >> i).max(1));
            let image = match self.unit {
                Unit::Block(bytes) => w.div_ceil(4) * h.div_ceil(4) * bytes,
                Unit::Bits(bits) => (w * bits).div_ceil(8) * h,
            };
            per_face += image * d;
        }
        (per_face * u64::from(self.faces() * self.array_size)) as usize
    }

    pub fn header(&self) -> Vec<u8> {
        let mut h = vec![0u8; self.header_size()];
        let put = |h: &mut Vec<u8>, pos: usize, v: u32| h[pos..pos + 4].copy_from_slice(&v.to_le_bytes());
        h[..4].copy_from_slice(b"DDS ");
        put(&mut h, 4, 124);
        let mut flags = 0x1007; // caps, height, width, pixel format
        if self.mips > 0 {
            flags |= 0x2_0000;
        }
        if self.depth > 1 {
            flags |= 0x80_0000;
        }
        put(&mut h, 8, flags);
        put(&mut h, 12, self.height);
        put(&mut h, 16, self.width);
        put(&mut h, 24, if self.depth > 1 { self.depth } else { 0 });
        put(&mut h, 28, self.mips);
        put(&mut h, 76, 32);
        match self.pf {
            Pf::FourCC(code) => {
                put(&mut h, 80, 0x4);
                h[84..88].copy_from_slice(&code);
            }
            Pf::Masks { flags, bits, masks } => {
                put(&mut h, 80, flags);
                put(&mut h, 88, bits);
                for (i, m) in masks.iter().enumerate() {
                    put(&mut h, 92 + 4 * i, *m);
                }
            }
            Pf::Dx10(format) => {
                put(&mut h, 80, 0x4);
                h[84..88].copy_from_slice(b"DX10");
                put(&mut h, 128, format);
                put(&mut h, 132, if self.depth > 1 { 4 } else { 3 });
                put(&mut h, 136, if self.cube { 0x4 } else { 0 });
                put(&mut h, 140, self.array_size);
            }
        }
        let mut caps = 0x1000;
        if self.mips > 1 || self.cube || self.depth > 1 {
            caps |= 0x8;
        }
        if self.mips > 1 {
            caps |= 0x40_0000;
        }
        put(&mut h, 108, caps);
        if !matches!(self.pf, Pf::Dx10(_)) {
            if self.cube {
                put(&mut h, 112, 0x200 | 0xFC00);
            } else if self.depth > 1 {
                put(&mut h, 112, 0x20_0000);
            }
        }
        h
    }

    /// Header plus random pixel data.
    pub fn build(&self, rng: &mut Rng) -> Vec<u8> {
        let mut out = self.header();
        out.extend(rng.bytes(self.data_size()));
        out
    }
}

/// A texture the scanner is expected to report.
#[derive(Debug, Clone)]
pub struct Expected {
    pub offset: usize,
    pub spec: DdsSpec,
    pub size: usize,
    pub crc32: u32,
}

/// A header the scanner should report as unusable, with how its reason starts.
#[derive(Debug, Clone)]
pub struct ExpectedReject {
    pub offset: usize,
    pub reason_prefix: &'static str,
}

#[derive(Debug, Clone)]
pub struct Fixture {
    pub name: &'static str,
    pub description: &'static str,
    pub data: Vec<u8>,
    pub expected: Vec<Expected>,
    pub rejected: Vec<ExpectedReject>,
}

impl Fixture {
    fn new(name: &'static str, description: &'static str) -> Self {
        Self { name, description, data: Vec::new(), expected: Vec::new(), rejected: Vec::new() }
    }

    fn texture(&mut self, spec: DdsSpec, rng: &mut Rng) {
        let bytes = spec.build(rng);
        self.expected.push(Expected { offset: self.data.len(), size: bytes.len(), crc32: crc32fast::hash(&bytes), spec });
        self.data.extend(bytes);
    }

    fn filler(&mut self, n: usize, rng: &mut Rng) {
        self.data.extend(rng.bytes(n));
    }
}

const RGB: u32 = 0x40;
const ALPHAPIXELS: u32 = 0x1;
const LUMINANCE: u32 = 0x2_0000;

pub fn dxt(code: &[u8; 4]) -> (Pf, Unit) {
    let bytes = if matches!(code, b"DXT1" | b"ATI1" | b"BC4U" | b"BC4S") { 8 } else { 16 };
    (Pf::FourCC(*code), Unit::Block(bytes))
}

/// Textures of every kind of DDS layout inside a made-up archive, with traps.
pub fn dds_archive() -> Fixture {
    let mut rng = Rng::new(0xDD5);
    let mut f = Fixture::new(
        "dds_archive",
        "DDS textures (legacy, DX10, mips, cube maps, volume, arrays, odd sizes) in an archive, plus traps",
    );
    f.data.extend_from_slice(b"PAK1");
    f.filler(60, &mut rng);
    // Trap: the magic in text. The header size field reads "file", not 124.
    f.data.extend_from_slice(b"Textures are stored as DDS files in this archive.\n");

    // A DXT1 texture whose pixel data contains a complete, valid DDS: found textures are
    // skipped past, so the inner one must not be reported.
    let (pf, unit) = dxt(b"DXT1");
    let outer = DdsSpec::new(256, 128, 9, pf, unit, "DXT1", Some(71));
    let mut bytes = outer.build(&mut rng);
    let (pf, unit) = dxt(b"DXT5");
    let inner = DdsSpec::new(8, 8, 1, pf, unit, "DXT5", Some(77)).build(&mut rng);
    bytes[1000..1000 + inner.len()].copy_from_slice(&inner);
    f.expected.push(Expected { offset: f.data.len(), size: bytes.len(), crc32: crc32fast::hash(&bytes), spec: outer });
    f.data.extend(bytes);
    f.filler(37, &mut rng);

    let (pf, unit) = dxt(b"DXT5");
    f.texture(DdsSpec::new(64, 64, 0, pf, unit, "DXT5", Some(77)), &mut rng);
    let argb = Pf::Masks { flags: RGB | ALPHAPIXELS, bits: 32, masks: [0xff_0000, 0xff00, 0xff, 0xff00_0000] };
    f.texture(DdsSpec::new(32, 16, 3, argb, Unit::Bits(32), "A8R8G8B8", Some(87)), &mut rng);
    f.filler(5, &mut rng);
    f.texture(DdsSpec::new(128, 128, 8, Pf::Dx10(99), Unit::Block(16), "BC7_UNORM_SRGB", Some(99)), &mut rng);
    let mut cube = DdsSpec::new(32, 32, 6, Pf::Dx10(71), Unit::Block(8), "BC1_UNORM", Some(71));
    cube.cube = true;
    f.texture(cube, &mut rng);
    let rgb565 = Pf::Masks { flags: RGB, bits: 16, masks: [0xf800, 0x7e0, 0x1f, 0] };
    let mut legacy_cube = DdsSpec::new(16, 16, 1, rgb565, Unit::Bits(16), "R5G6B5", Some(85));
    legacy_cube.cube = true;
    f.texture(legacy_cube, &mut rng);
    f.filler(11, &mut rng);
    let l8 = Pf::Masks { flags: LUMINANCE, bits: 8, masks: [0xff, 0, 0, 0] };
    let mut volume = DdsSpec::new(16, 16, 5, l8, Unit::Bits(8), "L8", Some(61));
    volume.depth = 8;
    f.texture(volume, &mut rng);
    f.texture(DdsSpec::new(40, 24, 6, Pf::Dx10(10), Unit::Bits(64), "R16G16B16A16_FLOAT", Some(10)), &mut rng);
    f.filler(64, &mut rng);
    // Odd sizes, then another texture straight after.
    let (pf, unit) = dxt(b"DXT5");
    f.texture(DdsSpec::new(100, 60, 7, pf, unit, "DXT5", Some(77)), &mut rng);
    let (pf, unit) = dxt(b"ATI2");
    f.texture(DdsSpec::new(8, 8, 4, pf, unit, "ATI2", Some(83)), &mut rng);
    let mut array = DdsSpec::new(16, 16, 5, Pf::Dx10(98), Unit::Block(16), "BC7_UNORM", Some(98));
    array.array_size = 3;
    f.texture(array, &mut rng);
    f.filler(20, &mut rng);

    // Trap: right header size, wrong pixel-format size. Not a DDS at all, so not reported.
    let (pf, unit) = dxt(b"DXT1");
    let mut not_dds = DdsSpec::new(16, 16, 1, pf, unit, "", None).build(&mut rng);
    not_dds[76] = 0;
    f.data.extend(not_dds);
    f.filler(9, &mut rng);
    // A DDS with a pixel format texscan doesn't know: reported as rejected.
    f.rejected.push(ExpectedReject { offset: f.data.len(), reason_prefix: "unknown FourCC" });
    f.data.extend(DdsSpec::new(16, 16, 1, Pf::FourCC(*b"ZZZZ"), Unit::Block(8), "", None).build(&mut rng));
    f.filler(33, &mut rng);
    // Cut off by the end of the file.
    f.rejected.push(ExpectedReject { offset: f.data.len(), reason_prefix: "truncated" });
    let (pf, unit) = dxt(b"DXT5");
    let mut cut = DdsSpec::new(256, 256, 1, pf, unit, "", None).build(&mut rng);
    cut.truncate(228);
    f.data.extend(cut);
    f
}

/// A plain `.dds` file: one texture at offset 0 and nothing else.
pub fn dds_file() -> Fixture {
    let mut rng = Rng::new(0xF11E);
    let mut f = Fixture::new("dds_file", "A single DX10 BC3 texture filling the whole file");
    f.texture(DdsSpec::new(64, 64, 7, Pf::Dx10(77), Unit::Block(16), "BC3_UNORM", Some(77)), &mut rng);
    f
}

pub fn all() -> Vec<Fixture> {
    vec![dds_archive(), dds_file()]
}
